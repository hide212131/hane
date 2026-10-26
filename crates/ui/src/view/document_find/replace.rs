//! Replace row of the document-find bar (Issue #589): the replacement field,
//! the target scope ("文書全体" or an explicitly captured "選択範囲内"), and the
//! single and replace-all operations. This is a child of `document_find` so it
//! shares the search state (session / generation / revision / query
//! generation identity, matches, current) instead of copying it.
//!
//! Atomicity and history: a replace-all is prepared on a background thread as
//! one covering edit (see `hane_editor::plan_replace_all`) over a shared rope
//! snapshot and published with one `Editor::replace_range_recorded` call after
//! every identity is re-checked on the UI thread. That call validates before it
//! mutates, records one Undo/Redo entry that does not merge with surrounding
//! typing, and runs through the normal `after_input` pipeline exactly once.
//!
//! Nothing here edits the document unless a replace operation succeeds; typing
//! in either field only changes bar state.

use super::*;
use hane_document::TextBuffer;
use hane_editor::{MAX_QUERY_BYTES, ReplacePlan, ReplacePlanError, plan_replace_all};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) enum ReplaceScopeMode {
    /// The whole current document (initial value).
    #[default]
    Document,
    /// Only the explicitly captured source range.
    Selection,
}

/// A body selection captured on request, kept apart from the live body
/// selection. `range` is a half-open source byte range; `target` is the
/// document identity it is valid for.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct ReplaceSelectionScope {
    pub(super) target: DocumentFindTarget,
    pub(super) range: SourceRange,
    /// Human-readable start/end position, computed once when the scope is
    /// captured or shifted (never per frame).
    pub(super) label: String,
}

/// What the last replace operation did, shown only while the document is
/// still the one (and the revision the one) it produced.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct ReplaceNotice {
    pub(super) target: DocumentFindTarget,
    pub(super) text: String,
}

/// After a single replacement, the next current match is chosen relative to
/// the text just inserted, so the inserted text is not immediately a target.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct PostReplaceAdvance {
    pub(super) target: DocumentFindTarget,
    pub(super) inserted: SourceRange,
}

/// Everything a prepared replace-all depends on. A result is applied only if
/// every part still equals the live value right before the edit.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ReplaceIdentity {
    target: DocumentFindTarget,
    query_generation: u64,
    scope_generation: u64,
    text_generation: u64,
    /// Bumped when a preparation is cancelled, so a plan that finished before
    /// the cancel but is delivered after it is discarded.
    cancel_generation: u64,
}

#[derive(Default)]
pub(super) struct DocumentReplaceState {
    pub(super) expanded: bool,
    pub(super) input: Option<Entity<InputState>>,
    pub(super) subscription: Option<Subscription>,
    pub(super) input_focused: bool,
    /// The committed replacement text (IME marked text is never stored here:
    /// the widget emits `Change` only for committed edits).
    pub(super) text: String,
    /// Bumped on every committed change of `text`.
    pub(super) text_generation: u64,
    /// See `ReplaceIdentity::cancel_generation`.
    pub(super) cancel_generation: u64,
    pub(super) scope_mode: ReplaceScopeMode,
    pub(super) scope: Option<ReplaceSelectionScope>,
    /// Bumped whenever the scope mode, captured range, or its validity
    /// changes, so a scan or plan prepared for an older scope is rejected even
    /// if the value later returns to what it was.
    pub(super) scope_generation: u64,
    /// See `DocumentFindState::escape_was_composing` / `enter_was_composing`;
    /// the same pre-dispatch snapshots for the replace field.
    pub(super) escape_was_composing: bool,
    pub(super) enter_was_composing: bool,
    /// A replace-all preparation task is outstanding (also while cancelled,
    /// until its task returns, so at most one runs at a time).
    pub(super) job_active: bool,
    pub(super) job_cancel: Option<Arc<AtomicBool>>,
    pub(super) notice: Option<ReplaceNotice>,
    /// The last paste into the replacement field carried a line break and was
    /// refused before the widget normalized it; `text` is still the older
    /// value, so replace stays blocked until a valid change or the row closes.
    pub(super) paste_rejected: bool,
    pub(super) post_advance: Option<PostReplaceAdvance>,
}

/// The match to make current after a single replacement that left
/// `inserted` (empty for a deletion) in the new text: the first match
/// starting at or after the inserted text's end; if none, the first match of
/// the document when it lies entirely before the inserted text; otherwise
/// none, so the next explicit Next/Previous chooses.
pub(super) fn index_after_replacement(
    matches: &[SourceRange],
    inserted: SourceRange,
) -> Option<usize> {
    let index = matches.partition_point(|m| m.start.0 < inserted.end.0);
    if index < matches.len() {
        return Some(index);
    }
    matches
        .first()
        .filter(|m| m.end.0 <= inserted.start.0)
        .map(|_| 0)
}

impl EditorView {
    fn initialize_document_replace_input(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.document_find.replace.input.is_some() {
            return;
        }
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("置換後"));
        let subscription = cx.subscribe_in(
            &input,
            window,
            |view, input, event: &InputEvent, window, cx| match event {
                InputEvent::Focus => {
                    view.document_find.replace.input_focused = true;
                    view.document_find.input_focused = false;
                    view.sidebar_keyboard_focus = false;
                    cx.notify();
                }
                InputEvent::Blur => {
                    view.document_find.replace.input_focused = false;
                    cx.notify();
                }
                InputEvent::Change => {
                    let text = input.read(cx).value().to_string();
                    view.document_replace_text_changed(text, cx);
                }
                // Enter replaces the current match unless it only confirmed
                // an IME composition (see `enter_was_composing`). Shift+Enter
                // never replaces; replace-all is button-only.
                InputEvent::PressEnter { shift, .. } => {
                    if !view.document_find.replace.enter_was_composing && !*shift {
                        view.document_replace_current(window, cx);
                    }
                }
            },
        );
        self.document_find.replace.input = Some(input);
        self.document_find.replace.subscription = Some(subscription);
    }

    /// A paste whose original clipboard text contains CR/LF was refused. The
    /// field value does not change (no `Change` event), so the attempt is
    /// recorded here and any prepared replace-all is invalidated.
    fn document_replace_paste_rejected(&mut self, cx: &mut Context<Self>) {
        let state = &mut self.document_find.replace;
        state.paste_rejected = true;
        state.text_generation = state.text_generation.wrapping_add(1);
        state.notice = None;
        if let Some(cancel) = state.job_cancel.take() {
            cancel.store(true, Ordering::Relaxed);
        }
        cx.notify();
    }

    fn document_replace_text_changed(&mut self, text: String, cx: &mut Context<Self>) {
        let state = &mut self.document_find.replace;
        state.paste_rejected = false;
        state.text = text;
        state.text_generation = state.text_generation.wrapping_add(1);
        state.notice = None;
        if let Some(cancel) = state.job_cancel.take() {
            cancel.store(true, Ordering::Relaxed);
        }
        cx.notify();
    }

    /// Shows or hides the replace row. Showing it focuses the replacement
    /// field; hiding it drops the captured scope (back to the whole
    /// document) and any preparation, and returns focus to the search field.
    pub(super) fn toggle_document_replace(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.document_find.open
            || self.editor().ime().is_some()
            || self.replace_fields_composing(window, cx)
        {
            return;
        }
        if self.document_find.replace.expanded {
            self.document_find.replace.expanded = false;
            self.document_find.replace.notice = None;
            if let Some(cancel) = self.document_find.replace.job_cancel.take() {
                cancel.store(true, Ordering::Relaxed);
            }
            let was_selection_mode =
                self.document_find.replace.scope_mode == ReplaceScopeMode::Selection;
            self.reset_document_replace_for_close();
            if let Some(input) = self.document_find.input.clone() {
                input.update(cx, |state, cx| state.focus(window, cx));
            }
            self.document_find.input_focused = true;
            self.document_find.replace.input_focused = false;
            if was_selection_mode {
                self.restart_document_find(cx, false, true);
            }
        } else {
            self.initialize_document_replace_input(window, cx);
            self.document_find.replace.expanded = true;
            if let Some(input) = self.document_find.replace.input.clone() {
                input.update(cx, |state, cx| state.focus(window, cx));
            }
            self.document_find.replace.input_focused = true;
            self.document_find.input_focused = false;
            self.sidebar_keyboard_focus = false;
        }
        cx.notify();
    }

    /// Flips between whole-document and selection scope. Switching to the
    /// selection scope captures the current non-empty body selection; with no
    /// selection (and no still-valid earlier capture) it shows guidance and
    /// does not touch the document.
    pub(super) fn toggle_document_replace_scope(&mut self, cx: &mut Context<Self>) {
        if self.document_find.replace.job_active {
            return;
        }
        let state = &mut self.document_find.replace;
        state.notice = None;
        state.scope_generation = state.scope_generation.wrapping_add(1);
        match state.scope_mode {
            ReplaceScopeMode::Document => {
                state.scope_mode = ReplaceScopeMode::Selection;
                self.try_capture_document_replace_scope();
            }
            ReplaceScopeMode::Selection => {
                state.scope_mode = ReplaceScopeMode::Document;
            }
        }
        self.restart_document_find(cx, false, true);
    }

    /// The "本文の選択を取り込む" button: (re)captures the body's current
    /// non-empty selection as the replace range and switches to it.
    pub(super) fn capture_document_replace_scope(&mut self, cx: &mut Context<Self>) {
        if self.document_find.replace.job_active {
            return;
        }
        self.document_find.replace.scope_mode = ReplaceScopeMode::Selection;
        if self.try_capture_document_replace_scope() {
            self.document_find.replace.notice = None;
            self.restart_document_find(cx, false, true);
        } else {
            self.set_replace_notice(
                "本文で範囲を選択してから「選択を取込」を押してください",
                cx,
            );
            self.restart_document_find(cx, false, true);
        }
    }

    /// Captures the body selection as the scope when it is non-empty and no
    /// body composition is open. Returns whether a new scope was captured; an
    /// earlier capture is otherwise left as it was.
    fn try_capture_document_replace_scope(&mut self) -> bool {
        if self.editor().ime().is_some() {
            return false;
        }
        let range = self.editor().selection().range();
        if range.is_empty() {
            return false;
        }
        let target = self.document_find_target();
        let label = self.document_replace_scope_label(range);
        let state = &mut self.document_find.replace;
        state.scope = Some(ReplaceSelectionScope {
            target,
            range,
            label,
        });
        state.scope_generation = state.scope_generation.wrapping_add(1);
        self.document_find.search_anchor = range.start;
        self.document_find.seeded_selection = None;
        true
    }

    fn document_replace_scope_label(&self, range: SourceRange) -> String {
        format!(
            "選択範囲内 {}〜{}",
            self.source_position_label(range.start),
            self.source_position_label(range.end)
        )
    }

    fn source_position_label(&self, offset: SourceOffset) -> String {
        let document = self.editor().document();
        let Ok(line) = document.line_for_offset(offset) else {
            return format!("{}バイト目", offset.0);
        };
        let Ok(line_range) = document.line_range(line) else {
            return format!("{}行", line.0 + 1);
        };
        let column = document
            .text(SourceRange::new(line_range.start.0, offset.0))
            .map_or(0, |text| text.chars().count());
        format!("{}行{}列", line.0 + 1, column + 1)
    }

    /// The captured range when it is usable for the current document: same
    /// identity, non-empty, and valid UTF-8 boundaries. `None` otherwise.
    pub(super) fn document_replace_scope_range(&self) -> Option<SourceRange> {
        let scope = self.document_find.replace.scope.as_ref()?;
        if scope.target != self.document_find_target() || scope.range.is_empty() {
            return None;
        }
        let document = self.editor().document();
        (document.validate_range(scope.range).is_ok()
            && document.validate_offset(scope.range.start).is_ok()
            && document.validate_offset(scope.range.end).is_ok())
        .then_some(scope.range)
    }

    /// Drops a captured range or pending advance that no longer belongs to
    /// the active document identity: any edit that is not this feature's own
    /// successful replacement, Undo/Redo, a reload, or a document switch.
    /// Called on every restart, which also covers a selection-only change
    /// correctly (it never restarts).
    pub(super) fn invalidate_stale_document_replace_state(&mut self) {
        let target = self.document_find_target();
        let state = &mut self.document_find.replace;
        if state.scope.as_ref().is_some_and(|s| s.target != target) {
            state.scope = None;
            state.scope_generation = state.scope_generation.wrapping_add(1);
        }
        if state.post_advance.is_some_and(|a| a.target != target) {
            state.post_advance = None;
        }
    }

    pub(super) fn take_document_replace_advance(
        &mut self,
        target: DocumentFindTarget,
    ) -> Option<SourceRange> {
        match self.document_find.replace.post_advance {
            Some(advance) if advance.target == target => {
                self.document_find.replace.post_advance = None;
                Some(advance.inserted)
            }
            _ => None,
        }
    }

    /// Cancels a replace-all preparation that is still running; returns
    /// whether there was one (the caller then keeps the bar open).
    pub(super) fn cancel_document_replace_preparation(&mut self, cx: &mut Context<Self>) -> bool {
        let state = &mut self.document_find.replace;
        if !state.job_active {
            return false;
        }
        let Some(cancel) = state.job_cancel.take() else {
            return false;
        };
        cancel.store(true, Ordering::Relaxed);
        state.cancel_generation = state.cancel_generation.wrapping_add(1);
        self.set_replace_notice("置換の準備を取り消しました", cx);
        true
    }

    /// Closing the bar discards the captured range, the scope mode, and any
    /// remembered result; the next open starts from the then-current text.
    pub(super) fn reset_document_replace_for_close(&mut self) {
        let state = &mut self.document_find.replace;
        state.scope = None;
        state.scope_mode = ReplaceScopeMode::Document;
        state.scope_generation = state.scope_generation.wrapping_add(1);
        state.notice = None;
        state.paste_rejected = false;
        state.post_advance = None;
    }

    fn set_replace_notice(&mut self, text: impl Into<String>, cx: &mut Context<Self>) {
        self.document_find.replace.notice = Some(ReplaceNotice {
            target: self.document_find_target(),
            text: text.into(),
        });
        cx.notify();
    }

    /// Whether the search field or the replace field has an active IME marked
    /// range, regardless of which one currently holds focus (a field that just
    /// lost focus to a button may still hold unconfirmed text).
    fn replace_fields_composing(&self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let inputs = [
            self.document_find.input.clone(),
            self.document_find.replace.input.clone(),
        ];
        for input in inputs.into_iter().flatten() {
            let composing = input.update(cx, |state, cx| {
                <InputState as gpui::EntityInputHandler>::marked_text_range(state, window, cx)
                    .is_some()
            });
            if composing {
                return true;
            }
        }
        false
    }

    fn document_replace_text_error(&self) -> Option<&'static str> {
        let text = &self.document_find.replace.text;
        if self.document_find.paste_rejected {
            Some("検索語に改行は使えません。単一行で入力し直してください")
        } else if self.document_find.replace.paste_rejected || text.contains(['\n', '\r']) {
            Some("置換後文字列に改行は使えません")
        } else if text.len() > MAX_QUERY_BYTES {
            Some("置換後文字列が長すぎます（4,096バイトまで）")
        } else {
            None
        }
    }

    fn document_replace_scope_name(&self) -> &'static str {
        match self.document_find.replace.scope_mode {
            ReplaceScopeMode::Document => "文書全体",
            ReplaceScopeMode::Selection => "選択範囲内",
        }
    }

    fn replace_identity(&self) -> ReplaceIdentity {
        ReplaceIdentity {
            target: self.document_find_target(),
            query_generation: self.document_find.query_generation,
            scope_generation: self.document_find.replace.scope_generation,
            text_generation: self.document_find.replace.text_generation,
            cancel_generation: self.document_find.replace.cancel_generation,
        }
    }

    /// Everything that must hold before a replace operation starts, checked
    /// by the handlers themselves (not only by the buttons' appearance).
    fn document_replace_gate(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), &'static str> {
        let find = &self.document_find;
        if !find.open {
            return Err("検索バーを開いてください");
        }
        if !self.body_editable() {
            return Err("閲覧モードでは置換できません");
        }
        if self.editor().ime().is_some() || self.replace_fields_composing(window, cx) {
            return Err("変換を確定してから実行してください");
        }
        if find.replace.job_active {
            return Err("置換を準備中です。完了または取消を待ってください");
        }
        if let Some(message) = self.document_replace_text_error() {
            return Err(message);
        }
        if find.replace.scope_mode == ReplaceScopeMode::Selection
            && self.document_replace_scope_range().is_none()
        {
            return Err("本文で範囲を選択して「選択を取込」を押してください");
        }
        if find.debounce_active
            || find.job_active
            || find.synced_target != Some(self.document_find_target())
            || !matches!(find.result, DocumentFindResult::Ready)
        {
            return Err("検索結果の更新を待っています");
        }
        Ok(())
    }

    /// "置換": replaces the current match once. The current match must still
    /// be in the scope and still match the query in the document right now.
    pub(crate) fn document_replace_current(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Err(message) = self.document_replace_gate(window, cx) {
            self.set_replace_notice(message, cx);
            return;
        }
        let Some(range) = self.document_find_current_match() else {
            self.set_replace_notice("置換する一致を前後移動で選んでください", cx);
            return;
        };
        let Ok(query) = FindQuery::parse(&self.document_find.query_text) else {
            return;
        };
        let options = FindOptions {
            case_sensitive: self.document_find.case_sensitive,
        };
        if self.document_find.replace.scope_mode == ReplaceScopeMode::Selection {
            let inside = self
                .document_replace_scope_range()
                .is_some_and(|scope| scope.start.0 <= range.start.0 && range.end.0 <= scope.end.0);
            if !inside {
                self.set_replace_notice("対象範囲が変わりました。検索し直します", cx);
                self.restart_document_find(cx, false, true);
                return;
            }
        }
        // Re-verify the range against the document itself; never recompute it
        // from the query's byte length (case-insensitive matches can differ).
        let verified = matches!(
            find_scan_range(self.editor().document(), &query, options, range, || false),
            Ok(FindScan::Completed(results)) if results.matches == [range]
        );
        let Ok(original) = self.editor().document().text(range) else {
            return;
        };
        if !verified {
            self.set_replace_notice("文書が変わったため検索し直します", cx);
            self.restart_document_find(cx, false, true);
            return;
        }
        let replacement = self.document_find.replace.text.clone();
        if original == replacement {
            // Same bytes: no edit, revision, dirty, or history change; just
            // move on so the same match is not current forever.
            self.set_replace_notice("変更なし（同じ文字列）", cx);
            self.step_document_find(true, cx);
            return;
        }
        let Some(caret) = range.start.0.checked_add(replacement.len()) else {
            return;
        };
        let selection_after = Selection::caret(SourceOffset(caret));
        let name = self.document_replace_scope_name();
        match self
            .editor_mut()
            .replace_range_recorded(range, &replacement, selection_after)
        {
            Ok(_) => {
                self.update_document_replace_scope_after_edit(range.len_bytes(), replacement.len());
                let advance = PostReplaceAdvance {
                    target: self.document_find_target(),
                    inserted: SourceRange::new(range.start.0, caret),
                };
                self.document_find.replace.post_advance = Some(advance);
                self.after_input(cx);
                self.set_replace_notice(format!("{name}: 1件を変更"), cx);
            }
            Err(error) => {
                self.set_replace_notice(
                    format!("置換に失敗しました。本文は変更されていません（{error:?}）"),
                    cx,
                );
            }
        }
    }

    /// "すべて置換": prepares the plan in the background for the current
    /// complete match list, then publishes it in `finish_document_replace_all`.
    pub(crate) fn document_replace_all(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Err(message) = self.document_replace_gate(window, cx) {
            self.set_replace_notice(message, cx);
            return;
        }
        if self.document_find.truncated {
            self.set_replace_notice(
                "100,000件を超えています。対象を狭めてください",
                cx,
            );
            return;
        }
        if self.document_find.matches.is_empty() {
            self.set_replace_notice("置換する一致がありません", cx);
            return;
        }
        let identity = self.replace_identity();
        // Shares the rope's persistent tree; nothing is copied on this thread.
        let buffer = self.editor().document().clone();
        let matches = self.document_find.matches.clone();
        let replacement = self.document_find.replace.text.clone();
        let cancel = Arc::new(AtomicBool::new(false));
        self.document_find.replace.notice = None;
        self.document_find.replace.job_active = true;
        self.document_find.replace.job_cancel = Some(cancel.clone());
        cx.spawn(async move |view, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    plan_replace_all(&buffer, &matches, &replacement, || {
                        cancel.load(Ordering::Relaxed)
                    })
                })
                .await;
            let _ = view.update(cx, |view, cx| {
                view.finish_document_replace_all(identity, result, cx);
            });
        })
        .detach();
        cx.notify();
    }

    fn finish_document_replace_all(
        &mut self,
        identity: ReplaceIdentity,
        result: Result<ReplacePlan, ReplacePlanError>,
        cx: &mut Context<Self>,
    ) {
        self.document_find.replace.job_active = false;
        self.document_find.replace.job_cancel = None;
        // Any query / scope / replacement / document change since the request
        // makes this plan stale: discard it, never rebase or re-run it.
        if !self.document_find.open || identity != self.replace_identity() {
            cx.notify();
            return;
        }
        match result {
            Ok(plan) => self.apply_document_replace_plan(plan, cx),
            Err(ReplacePlanError::Cancelled) => {
                self.set_replace_notice("置換の準備を取り消しました", cx);
            }
            Err(error) => {
                self.set_replace_notice(
                    format!("置換を準備できませんでした。本文は変更されていません（{error:?}）"),
                    cx,
                );
            }
        }
    }

    fn apply_document_replace_plan(&mut self, plan: ReplacePlan, cx: &mut Context<Self>) {
        if !self.body_editable() {
            self.set_replace_notice("閲覧モードでは置換できません", cx);
            return;
        }
        if self.editor().ime().is_some() {
            self.set_replace_notice("変換を確定してから実行してください", cx);
            return;
        }
        if plan.source_len != self.editor().document().len_bytes().0 {
            self.set_replace_notice("文書が変わったため検索し直します", cx);
            self.restart_document_find(cx, false, true);
            return;
        }
        let name = self.document_replace_scope_name();
        let Some(range) = plan.range else {
            self.set_replace_notice(
                format!(
                    "{name}: 一致{}件はすべて同じ文字列のため変更なし",
                    plan.matched
                ),
                cx,
            );
            return;
        };
        if self.document_find.replace.scope_mode == ReplaceScopeMode::Selection {
            let inside = self
                .document_replace_scope_range()
                .is_some_and(|scope| scope.start.0 <= range.start.0 && range.end.0 <= scope.end.0);
            if !inside {
                self.set_replace_notice("対象範囲が変わりました。検索し直します", cx);
                self.restart_document_find(cx, false, true);
                return;
            }
        }
        let Some(caret) = range.start.0.checked_add(plan.replacement.len()) else {
            self.set_replace_notice("置換後のサイズが大きすぎます", cx);
            return;
        };
        let selection_after = Selection::caret(SourceOffset(caret));
        match self
            .editor_mut()
            .replace_range_recorded(range, &plan.replacement, selection_after)
        {
            Ok(_) => {
                self.update_document_replace_scope_after_edit(
                    plan.deleted_bytes,
                    plan.inserted_bytes,
                );
                self.document_find.replace.post_advance = None;
                self.after_input(cx);
                let text = if plan.matched == plan.changed {
                    format!("{name}: {}件を変更", plan.changed)
                } else {
                    format!("{name}: 一致{}件・変更{}件", plan.matched, plan.changed)
                };
                self.set_replace_notice(text, cx);
            }
            Err(error) => {
                self.set_replace_notice(
                    format!("置換に失敗しました。本文は変更されていません（{error:?}）"),
                    cx,
                );
            }
        }
    }

    /// Re-binds the captured range to the new revision after this feature's
    /// own successful edit: the start stays and the end moves by the bytes
    /// actually inserted minus deleted. In whole-document mode the captured
    /// range (if any) can no longer be trusted and is dropped. Must run
    /// before `after_input` so the resync does not take it for an outside
    /// edit.
    fn update_document_replace_scope_after_edit(&mut self, deleted: usize, inserted: usize) {
        let target = self.document_find_target();
        let previous = self.document_find.replace.scope.take();
        let mode = self.document_find.replace.scope_mode;
        let generation = self.document_find.replace.scope_generation;
        self.document_find.replace.scope_generation = generation.wrapping_add(1);
        if mode != ReplaceScopeMode::Selection {
            return;
        }
        let Some(previous) = previous else {
            return;
        };
        let Some(end) = previous
            .range
            .end
            .0
            .checked_sub(deleted)
            .and_then(|end| end.checked_add(inserted))
        else {
            return;
        };
        if end < previous.range.start.0 {
            return;
        }
        let range = SourceRange::new(previous.range.start.0, end);
        if self.editor().document().validate_range(range).is_err() {
            return;
        }
        let label = self.document_replace_scope_label(range);
        self.document_find.replace.scope = Some(ReplaceSelectionScope {
            target,
            range,
            label,
        });
    }

    /// Whether the buttons look enabled; the handlers re-check everything.
    fn document_replace_looks_runnable(&self, all: bool) -> bool {
        let find = &self.document_find;
        let base = find.open
            && self.body_editable()
            && !find.replace.job_active
            && !find.debounce_active
            && !find.job_active
            && matches!(find.result, DocumentFindResult::Ready)
            && self.document_replace_text_error().is_none();
        if !base {
            return false;
        }
        if all {
            !find.truncated && !find.matches.is_empty()
        } else {
            find.current.is_some()
        }
    }

    fn document_replace_info_text(&self) -> String {
        let state = &self.document_find.replace;
        let target = self.document_find_target();
        if let Some(notice) = state.notice.as_ref()
            && notice.target == target
        {
            return notice.text.clone();
        }
        if state.job_active {
            return "置換を準備中…（Escで取消）".to_owned();
        }
        if let Some(message) = self.document_replace_text_error() {
            return message.to_owned();
        }
        if self.document_find.truncated
            && matches!(self.document_find.result, DocumentFindResult::Ready)
        {
            return "100,000件を超えています。対象を狭めてください".to_owned();
        }
        match state.scope_mode {
            ReplaceScopeMode::Document => "対象: 文書全体".to_owned(),
            ReplaceScopeMode::Selection => match &state.scope {
                Some(scope) if scope.target == target && !scope.range.is_empty() => {
                    scope.label.clone()
                }
                _ => "本文で範囲を選択して「選択を取込」を押してください".to_owned(),
            },
        }
    }

    pub(super) fn document_replace_button(
        &self,
        id: &'static str,
        label: &'static str,
        enabled: bool,
    ) -> gpui::Stateful<gpui::Div> {
        div()
            .id(id)
            .debug_selector(move || id.to_owned())
            .h(px(DOCUMENT_FIND_BAR_HEIGHT))
            .px(px(6.0))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .rounded_sm()
            .cursor_pointer()
            .bg(rgb(self.theme.code_background))
            .text_color(rgb(self.theme.foreground))
            .opacity(if enabled { 1.0 } else { 0.45 })
            .child(label)
    }

    pub(super) fn document_replace_row(
        &self,
        cx: &mut Context<Self>,
    ) -> Option<gpui::Stateful<gpui::Div>> {
        if !self.document_find.replace.expanded {
            return None;
        }
        let input = self.document_find.replace.input.as_ref()?;
        let view = cx.entity().downgrade();
        let replace_input = Input::new(input)
            .on_paste(move |item, _, cx| {
                if !clipboard_has_line_break(item) {
                    return false;
                }
                view.update(cx, |view, cx| view.document_replace_paste_rejected(cx))
                    .is_ok()
            })
            .id("document-replace-input")
            .h(px(DOCUMENT_FIND_BAR_HEIGHT))
            .appearance(false)
            .bordered(false)
            .focus_bordered(false);
        let replace_box = div()
            .id("document-replace-field")
            .h(px(DOCUMENT_FIND_BAR_HEIGHT))
            .flex_1()
            .min_w(px(80.0))
            .px(px(4.0))
            .rounded_sm()
            .border_1()
            .border_color(rgb(self.theme.sidebar_foreground))
            .bg(rgb(self.theme.code_background))
            .text_color(rgb(self.theme.foreground))
            .child(replace_input);
        let scope_button = self
            .document_replace_button(
                "document-replace-scope",
                match self.document_find.replace.scope_mode {
                    ReplaceScopeMode::Document => "対象: 文書全体",
                    ReplaceScopeMode::Selection => "対象: 選択範囲内",
                },
                !self.document_find.replace.job_active,
            )
            .on_click(cx.listener(|view, _, _, cx| view.toggle_document_replace_scope(cx)));
        let capture_button = self
            .document_replace_button(
                "document-replace-capture",
                "選択を取込",
                !self.document_find.replace.job_active,
            )
            .on_click(cx.listener(|view, _, _, cx| view.capture_document_replace_scope(cx)));
        let one_button = self
            .document_replace_button(
                "document-replace-one",
                "置換",
                self.document_replace_looks_runnable(false),
            )
            .on_click(cx.listener(|view, _, window, cx| view.document_replace_current(window, cx)));
        let all_button = self
            .document_replace_button(
                "document-replace-all",
                "すべて置換",
                self.document_replace_looks_runnable(true),
            )
            .on_click(cx.listener(|view, _, window, cx| view.document_replace_all(window, cx)));
        let info = div()
            .id("document-replace-info")
            .debug_selector(|| "document-replace-info".to_owned())
            .h(px(DOCUMENT_FIND_BAR_HEIGHT))
            .max_w(px(280.0))
            .px(px(6.0))
            .flex_none()
            .flex()
            .items_center()
            .truncate()
            .child(self.document_replace_info_text());
        Some(
            div()
                .id("document-replace-row")
                .flex_none()
                .w_full()
                .h(px(DOCUMENT_FIND_BAR_HEIGHT + SIDEBAR_FILTER_GAP * 2.0))
                .flex()
                .items_center()
                .gap_2()
                .px_2()
                .py(px(SIDEBAR_FILTER_GAP))
                .child(replace_box)
                .child(scope_button)
                .child(capture_button)
                .child(one_button)
                .child(all_button)
                .child(info),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open_find(
        view: &Entity<EditorView>,
        cx: &mut gpui::VisualTestContext,
        query: &str,
        replacement: &str,
    ) {
        cx.update(|window, app| {
            view.update(app, |view, cx| view.open_document_find(window, cx));
        });
        cx.run_until_parked();
        view.update(cx, |view, cx| {
            view.document_find.query_text = query.to_owned();
            view.document_find.replace.text = replacement.to_owned();
            view.restart_document_find(cx, false, true);
        });
        cx.run_until_parked();
    }

    fn replace_all(view: &Entity<EditorView>, cx: &mut gpui::VisualTestContext) {
        cx.update(|window, app| {
            view.update(app, |view, cx| view.document_replace_all(window, cx));
        });
        cx.run_until_parked();
    }

    fn replace_current(view: &Entity<EditorView>, cx: &mut gpui::VisualTestContext) {
        cx.update(|window, app| {
            view.update(app, |view, cx| view.document_replace_current(window, cx));
        });
        cx.run_until_parked();
    }

    fn settle_debounce(cx: &mut gpui::VisualTestContext) {
        // See `rapid_query_changes_coalesce_into_a_single_scan_for_the_final_text`
        // for why both a real sleep and a virtual clock advance are needed.
        std::thread::sleep(DOCUMENT_FIND_DEBOUNCE + Duration::from_millis(50));
        cx.executor()
            .advance_clock(DOCUMENT_FIND_DEBOUNCE + Duration::from_millis(50));
        cx.run_until_parked();
    }

    fn text(view: &Entity<EditorView>, cx: &mut gpui::VisualTestContext) -> String {
        view.read_with(cx, |view, _| view.editor().document().full_text())
    }

    fn notice(view: &Entity<EditorView>, cx: &mut gpui::VisualTestContext) -> String {
        view.read_with(cx, |view, _| view.document_replace_info_text())
    }

    #[gpui::test]
    fn replace_all_is_one_undo_step_and_reports_the_changed_count(cx: &mut gpui::TestAppContext) {
        cx.update(crate::actions::register_key_bindings);
        let (view, cx) = cx.add_window_view(|_, cx| EditorView::new("foo bar foo\n", "Untitled", cx));
        cx.simulate_resize(gpui::size(px(960.0), px(760.0)));
        cx.run_until_parked();

        open_find(&view, cx, "foo", "baz");
        replace_all(&view, cx);
        assert_eq!(text(&view, cx), "baz bar baz\n");
        assert_eq!(notice(&view, cx), "文書全体: 2件を変更");

        view.update(cx, |view, cx| view.dispatch(EditorCommand::Undo, cx));
        assert_eq!(text(&view, cx), "foo bar foo\n");
        view.update(cx, |view, cx| view.dispatch(EditorCommand::Redo, cx));
        assert_eq!(text(&view, cx), "baz bar baz\n");
    }

    #[gpui::test]
    fn replace_all_with_an_empty_replacement_deletes_the_matches(cx: &mut gpui::TestAppContext) {
        cx.update(crate::actions::register_key_bindings);
        let (view, cx) = cx.add_window_view(|_, cx| EditorView::new("a-b-c\n", "Untitled", cx));
        cx.simulate_resize(gpui::size(px(960.0), px(760.0)));
        cx.run_until_parked();

        open_find(&view, cx, "-", "");
        replace_all(&view, cx);
        assert_eq!(text(&view, cx), "abc\n");
    }

    #[gpui::test]
    fn replace_all_does_not_search_its_own_inserted_text(cx: &mut gpui::TestAppContext) {
        cx.update(crate::actions::register_key_bindings);
        let (view, cx) = cx.add_window_view(|_, cx| EditorView::new("a a\n", "Untitled", cx));
        cx.simulate_resize(gpui::size(px(960.0), px(760.0)));
        cx.run_until_parked();

        open_find(&view, cx, "a", "aa");
        replace_all(&view, cx);
        assert_eq!(text(&view, cx), "aa aa\n");
    }

    #[gpui::test]
    fn replace_all_with_nothing_to_change_keeps_revision_dirty_and_redo(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(crate::actions::register_key_bindings);
        let (view, cx) = cx.add_window_view(|_, cx| EditorView::new("same\n", "Untitled", cx));
        cx.simulate_resize(gpui::size(px(960.0), px(760.0)));
        cx.run_until_parked();

        // Build an undone entry first so a redo exists to be preserved.
        view.update(cx, |view, cx| {
            view.insert_text("x", cx);
            view.dispatch(EditorCommand::Undo, cx);
        });
        let revision = view.read_with(cx, |view, _| view.sessions.active().revision());
        assert!(view.read_with(cx, |view, _| view.editor().can_redo()));

        open_find(&view, cx, "same", "same");
        replace_all(&view, cx);

        view.read_with(cx, |view, _| {
            assert_eq!(view.sessions.active().revision(), revision);
            assert!(view.editor().can_redo());
            assert_eq!(view.editor().document().full_text(), "same\n");
        });
        assert!(notice(&view, cx).contains("変更なし"));
    }

    #[gpui::test]
    fn replace_all_in_a_selection_scope_leaves_the_rest_untouched(cx: &mut gpui::TestAppContext) {
        cx.update(crate::actions::register_key_bindings);
        let (view, cx) =
            cx.add_window_view(|_, cx| EditorView::new("foo\nfoo\nfoo\n", "Untitled", cx));
        cx.simulate_resize(gpui::size(px(960.0), px(760.0)));
        cx.run_until_parked();

        // Select the middle line, then open the bar (which would seed from a
        // single-line selection) and switch to the selection scope.
        view.update(cx, |view, cx| {
            view.editor_mut()
                .set_selection(Selection {
                    anchor: SourceOffset(4),
                    active: SourceOffset(7),
                })
                .unwrap();
            cx.notify();
        });
        open_find(&view, cx, "foo", "ab");
        view.update(cx, |view, cx| view.toggle_document_replace_scope(cx));
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert_eq!(view.document_find.matches, vec![SourceRange::new(4, 7)]);
        });

        replace_all(&view, cx);
        assert_eq!(text(&view, cx), "foo\nab\nfoo\n");
        assert_eq!(notice(&view, cx), "選択範囲内: 1件を変更");
        view.read_with(cx, |view, _| {
            // Start kept, end moved by the actual byte delta (-1).
            assert_eq!(
                view.document_find.replace.scope.as_ref().map(|s| s.range),
                Some(SourceRange::new(4, 6))
            );
        });

        view.update(cx, |view, cx| view.dispatch(EditorCommand::Undo, cx));
        assert_eq!(text(&view, cx), "foo\nfoo\nfoo\n");
    }

    #[gpui::test]
    fn selection_scope_without_a_capture_never_falls_back_to_the_whole_document(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(crate::actions::register_key_bindings);
        let (view, cx) = cx.add_window_view(|_, cx| EditorView::new("foo foo\n", "Untitled", cx));
        cx.simulate_resize(gpui::size(px(960.0), px(760.0)));
        cx.run_until_parked();

        open_find(&view, cx, "foo", "bar");
        view.update(cx, |view, cx| view.toggle_document_replace_scope(cx));
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert_eq!(view.document_find.result, DocumentFindResult::NeedsScope);
        });

        replace_all(&view, cx);
        replace_current(&view, cx);
        assert_eq!(text(&view, cx), "foo foo\n");
    }

    #[gpui::test]
    fn an_outside_edit_invalidates_the_captured_scope(cx: &mut gpui::TestAppContext) {
        cx.update(crate::actions::register_key_bindings);
        let (view, cx) =
            cx.add_window_view(|_, cx| EditorView::new("foo\nfoo\n", "Untitled", cx));
        cx.simulate_resize(gpui::size(px(960.0), px(760.0)));
        cx.run_until_parked();

        view.update(cx, |view, cx| {
            view.editor_mut()
                .set_selection(Selection {
                    anchor: SourceOffset(4),
                    active: SourceOffset(7),
                })
                .unwrap();
            cx.notify();
        });
        open_find(&view, cx, "foo", "bar");
        view.update(cx, |view, cx| view.toggle_document_replace_scope(cx));
        cx.run_until_parked();
        assert!(view.read_with(cx, |view, _| view.document_replace_scope_range().is_some()));

        view.update(cx, |view, cx| {
            view.editor_mut()
                .set_selection(Selection::caret(SourceOffset(0)))
                .unwrap();
            view.insert_text("x", cx);
        });
        settle_debounce(cx);
        view.read_with(cx, |view, _| {
            assert!(view.document_replace_scope_range().is_none());
            assert_eq!(view.document_find.result, DocumentFindResult::NeedsScope);
        });
        replace_all(&view, cx);
        assert_eq!(text(&view, cx), "xfoo\nfoo\n");
    }

    #[gpui::test]
    fn a_plan_prepared_before_an_edit_is_discarded_not_applied(cx: &mut gpui::TestAppContext) {
        cx.update(crate::actions::register_key_bindings);
        let (view, cx) = cx.add_window_view(|_, cx| EditorView::new("foo foo\n", "Untitled", cx));
        cx.simulate_resize(gpui::size(px(960.0), px(760.0)));
        cx.run_until_parked();

        open_find(&view, cx, "foo", "bar");
        cx.update(|window, app| {
            view.update(app, |view, cx| {
                view.document_replace_all(window, cx);
                assert!(view.document_find.replace.job_active);
                // The document changes before the background plan returns.
                view.editor_mut()
                    .set_selection(Selection::caret(SourceOffset(0)))
                    .unwrap();
                view.insert_text("z", cx);
            });
        });
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert!(!view.document_find.replace.job_active);
        });
        assert_eq!(text(&view, cx), "zfoo foo\n");
    }

    /// A plan that was already completed in the background when the cancel was
    /// accepted must not be published by its later UI continuation. The
    /// delivery order is injected deterministically (no timing dependence).
    #[gpui::test]
    fn a_completed_plan_delivered_after_a_successful_cancel_is_discarded(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(crate::actions::register_key_bindings);
        let (view, cx) = cx.add_window_view(|_, cx| EditorView::new("foo foo\n", "Untitled", cx));
        cx.simulate_resize(gpui::size(px(960.0), px(760.0)));
        cx.run_until_parked();

        open_find(&view, cx, "foo", "bar");
        let before = body_state(&view, cx);
        let dirty_before = view.read_with(cx, |view, _| view.sessions.active().is_dirty());
        let scope_before = view.read_with(cx, |view, _| {
            (
                view.document_find.replace.scope_mode,
                view.document_find.replace.scope.clone(),
            )
        });

        let (identity, plan) = view.read_with(cx, |view, _| {
            let plan = plan_replace_all(
                view.editor().document(),
                &view.document_find.matches,
                "bar",
                || false,
            )
            .unwrap();
            (view.replace_identity(), plan)
        });
        // The completed background result waits for its UI continuation.
        view.update(cx, |view, cx| {
            view.document_find.replace.job_active = true;
            view.document_find.replace.job_cancel = Some(Arc::new(AtomicBool::new(false)));
            assert!(view.cancel_document_replace_preparation(cx));
            assert!(view.document_replace_info_text().contains("取り消しました"));
            view.finish_document_replace_all(identity, Ok(plan), cx);
        });

        assert_eq!(text(&view, cx), "foo foo\n");
        assert_eq!(body_state(&view, cx), before);
        view.read_with(cx, |view, _| {
            assert_eq!(view.sessions.active().is_dirty(), dirty_before);
            assert_eq!(
                (
                    view.document_find.replace.scope_mode,
                    view.document_find.replace.scope.clone()
                ),
                scope_before
            );
            assert!(!view.document_find.replace.job_active);
            assert!(view.document_find.open);
        });
        assert!(notice(&view, cx).contains("取り消しました"));

        // A new, independent request after the cancel still succeeds.
        replace_all(&view, cx);
        assert_eq!(text(&view, cx), "bar bar\n");
        assert_eq!(notice(&view, cx), "文書全体: 2件を変更");
        view.update(cx, |view, cx| view.dispatch(EditorCommand::Undo, cx));
        assert_eq!(text(&view, cx), "foo foo\n");
    }

    #[gpui::test]
    fn a_normal_completion_without_cancel_still_publishes_its_plan(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(crate::actions::register_key_bindings);
        let (view, cx) = cx.add_window_view(|_, cx| EditorView::new("foo foo\n", "Untitled", cx));
        cx.simulate_resize(gpui::size(px(960.0), px(760.0)));
        cx.run_until_parked();

        open_find(&view, cx, "foo", "bar");
        let (identity, plan) = view.read_with(cx, |view, _| {
            let plan = plan_replace_all(
                view.editor().document(),
                &view.document_find.matches,
                "bar",
                || false,
            )
            .unwrap();
            (view.replace_identity(), plan)
        });
        view.update(cx, |view, cx| {
            view.document_find.replace.job_active = true;
            view.document_find.replace.job_cancel = Some(Arc::new(AtomicBool::new(false)));
            view.finish_document_replace_all(identity, Ok(plan), cx);
        });
        assert_eq!(text(&view, cx), "bar bar\n");
        assert!(view.read_with(cx, |view, _| view.editor().can_undo()));
    }

    #[gpui::test]
    fn changing_the_replacement_while_preparing_discards_the_plan(cx: &mut gpui::TestAppContext) {
        cx.update(crate::actions::register_key_bindings);
        let (view, cx) = cx.add_window_view(|_, cx| EditorView::new("foo foo\n", "Untitled", cx));
        cx.simulate_resize(gpui::size(px(960.0), px(760.0)));
        cx.run_until_parked();

        open_find(&view, cx, "foo", "bar");
        cx.update(|window, app| {
            view.update(app, |view, cx| {
                view.document_replace_all(window, cx);
                view.document_replace_text_changed("qux".to_owned(), cx);
            });
        });
        cx.run_until_parked();
        assert_eq!(text(&view, cx), "foo foo\n");
    }

    #[gpui::test]
    fn single_replace_moves_past_the_inserted_text(cx: &mut gpui::TestAppContext) {
        cx.update(crate::actions::register_key_bindings);
        let (view, cx) = cx.add_window_view(|_, cx| EditorView::new("a a\n", "Untitled", cx));
        cx.simulate_resize(gpui::size(px(960.0), px(760.0)));
        cx.run_until_parked();

        open_find(&view, cx, "a", "aa");
        view.read_with(cx, |view, _| {
            assert_eq!(view.document_find_current_match(), Some(SourceRange::new(0, 1)));
        });
        replace_current(&view, cx);
        assert_eq!(text(&view, cx), "aa a\n");
        settle_debounce(cx);
        view.read_with(cx, |view, _| {
            assert_eq!(view.document_find_current_match(), Some(SourceRange::new(3, 4)));
            assert_eq!(
                view.editor().selection(),
                Selection::caret(SourceOffset(2))
            );
        });
        view.update(cx, |view, cx| view.dispatch(EditorCommand::Undo, cx));
        assert_eq!(text(&view, cx), "a a\n");
    }

    #[gpui::test]
    fn single_delete_selects_the_match_that_closed_up(cx: &mut gpui::TestAppContext) {
        cx.update(crate::actions::register_key_bindings);
        let (view, cx) = cx.add_window_view(|_, cx| EditorView::new("xaya\n", "Untitled", cx));
        cx.simulate_resize(gpui::size(px(960.0), px(760.0)));
        cx.run_until_parked();

        open_find(&view, cx, "a", "");
        replace_current(&view, cx);
        assert_eq!(text(&view, cx), "xya\n");
        settle_debounce(cx);
        view.read_with(cx, |view, _| {
            assert_eq!(view.document_find_current_match(), Some(SourceRange::new(2, 3)));
        });
    }

    #[gpui::test]
    fn single_replace_with_identical_bytes_changes_nothing_and_advances(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(crate::actions::register_key_bindings);
        let (view, cx) = cx.add_window_view(|_, cx| EditorView::new("ab ab\n", "Untitled", cx));
        cx.simulate_resize(gpui::size(px(960.0), px(760.0)));
        cx.run_until_parked();

        let revision = view.read_with(cx, |view, _| view.sessions.active().revision());
        open_find(&view, cx, "ab", "ab");
        replace_current(&view, cx);
        view.read_with(cx, |view, _| {
            assert_eq!(view.sessions.active().revision(), revision);
            assert!(!view.editor().can_undo());
            assert_eq!(view.document_find.current, Some(1));
        });
    }

    #[gpui::test]
    fn a_replacement_with_a_line_break_is_refused_without_editing(cx: &mut gpui::TestAppContext) {
        cx.update(crate::actions::register_key_bindings);
        let (view, cx) = cx.add_window_view(|_, cx| EditorView::new("foo\n", "Untitled", cx));
        cx.simulate_resize(gpui::size(px(960.0), px(760.0)));
        cx.run_until_parked();

        open_find(&view, cx, "foo", "a\nb");
        replace_all(&view, cx);
        replace_current(&view, cx);
        assert_eq!(text(&view, cx), "foo\n");
        assert_eq!(notice(&view, cx), "置換後文字列に改行は使えません");
    }

    fn paste(cx: &mut gpui::VisualTestContext, clipboard: &str) {
        cx.write_to_clipboard(gpui::ClipboardItem::new_string(clipboard.to_owned()));
        cx.simulate_keystrokes("secondary-v");
        cx.run_until_parked();
    }

    fn body_state(
        view: &Entity<EditorView>,
        cx: &mut gpui::VisualTestContext,
    ) -> (String, Selection, String, bool, bool) {
        view.read_with(cx, |view, _| {
            (
                view.editor().document().full_text(),
                view.editor().selection(),
                format!("{:?}", view.sessions.active().revision()),
                view.editor().can_undo(),
                view.editor().can_redo(),
            )
        })
    }

    /// Real clipboard → real Paste keystroke → the rendered `Input`'s
    /// `on_paste`, for the replacement field (the pasted line break must be
    /// refused, never joined into `ab`).
    #[gpui::test]
    fn a_line_break_paste_into_the_replace_field_is_refused_and_recoverable(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(crate::actions::register_key_bindings);
        let (view, cx) = cx.add_window_view(|_, cx| EditorView::new("foo foo\n", "Untitled", cx));
        cx.simulate_resize(gpui::size(px(960.0), px(760.0)));
        cx.run_until_parked();

        open_find(&view, cx, "foo", "bar");
        cx.update(|window, app| {
            view.update(app, |view, cx| view.toggle_document_replace(window, cx));
        });
        cx.run_until_parked();
        let before = body_state(&view, cx);

        for payload in ["a\nb", "a\rb", "a\r\nb"] {
            let generation = view.read_with(cx, |view, _| view.document_find.replace.text_generation);
            // A plan prepared before the refused paste must never publish.
            cx.update(|window, app| {
                view.update(app, |view, cx| view.document_replace_all(window, cx));
            });
            paste(cx, payload);
            view.read_with(cx, |view, _| {
                assert!(view.document_find.replace.paste_rejected, "{payload:?}");
                assert_ne!(view.document_find.replace.text_generation, generation);
                assert_eq!(view.document_find.replace.text, "bar", "{payload:?}");
                assert!(!view.document_find.replace.job_active, "{payload:?}");
            });
            replace_all(&view, cx);
            replace_current(&view, cx);
            assert_eq!(body_state(&view, cx), before, "{payload:?}");
            assert_eq!(notice(&view, cx), "置換後文字列に改行は使えません");
            let input_value = view.read_with(cx, |view, cx| {
                view.document_find
                    .replace
                    .input
                    .as_ref()
                    .map(|input| input.read(cx).value().to_string())
            });
            assert_ne!(input_value.as_deref(), Some("ab"), "{payload:?}");
        }

        // A valid single-line paste recovers.
        paste(cx, "baz");
        view.read_with(cx, |view, _| {
            assert!(!view.document_find.replace.paste_rejected);
            assert_eq!(view.document_find.replace.text, "baz");
        });
        replace_all(&view, cx);
        assert_eq!(text(&view, cx), "baz baz\n");
    }

    /// Same contract for the search field: the refused line break leaves the
    /// old query, blocks replace, and a valid paste recovers.
    #[gpui::test]
    fn a_line_break_paste_into_the_search_field_is_refused_and_recoverable(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(crate::actions::register_key_bindings);
        let (view, cx) = cx.add_window_view(|_, cx| EditorView::new("foo foo\n", "Untitled", cx));
        cx.simulate_resize(gpui::size(px(960.0), px(760.0)));
        cx.run_until_parked();

        open_find(&view, cx, "foo", "bar");
        let input = view.read_with(cx, |view, _| view.document_find.input.clone().unwrap());
        cx.update(|window, app| {
            input.update(app, |state, cx| state.focus(window, cx));
        });
        cx.run_until_parked();
        let before = body_state(&view, cx);

        for payload in ["a\nb", "a\rb", "a\r\nb"] {
            let generation = view.read_with(cx, |view, _| view.document_find.query_generation);
            cx.update(|window, app| {
                view.update(app, |view, cx| view.document_replace_all(window, cx));
            });
            paste(cx, payload);
            view.read_with(cx, |view, _| {
                assert!(view.document_find.paste_rejected, "{payload:?}");
                assert_ne!(view.document_find.query_generation, generation);
                assert_eq!(view.document_find.query_text, "foo", "{payload:?}");
                assert!(view.document_find.replace.job_cancel.is_none(), "{payload:?}");
            });
            replace_all(&view, cx);
            replace_current(&view, cx);
            assert_eq!(body_state(&view, cx), before, "{payload:?}");
            assert_eq!(
                notice(&view, cx),
                "検索語に改行は使えません。単一行で入力し直してください"
            );
        }

        paste(cx, "foo");
        settle_debounce(cx);
        view.read_with(cx, |view, _| {
            assert!(!view.document_find.paste_rejected);
            assert_eq!(view.document_find.query_text, "foo");
        });
        replace_all(&view, cx);
        assert_eq!(text(&view, cx), "bar bar\n");
    }

    #[gpui::test]
    fn an_over_limit_match_list_disables_replace_all(cx: &mut gpui::TestAppContext) {
        cx.update(crate::actions::register_key_bindings);
        let body = "a".repeat(hane_editor::MAX_MATCHES + 1);
        let (view, cx) = cx.add_window_view(move |_, cx| EditorView::new(&body, "Untitled", cx));
        cx.simulate_resize(gpui::size(px(960.0), px(760.0)));
        cx.run_until_parked();

        open_find(&view, cx, "a", "b");
        view.read_with(cx, |view, _| assert!(view.document_find.truncated));
        replace_all(&view, cx);
        assert!(text(&view, cx).chars().all(|c| c == 'a'));
        assert_eq!(notice(&view, cx), "100,000件を超えています。対象を狭めてください");
    }

    #[gpui::test]
    fn the_replace_field_shields_the_body_from_editing_keys(cx: &mut gpui::TestAppContext) {
        cx.update(crate::actions::register_key_bindings);
        let (view, cx) = cx.add_window_view(|_, cx| EditorView::new("body\n", "Untitled", cx));
        cx.simulate_resize(gpui::size(px(960.0), px(760.0)));
        cx.run_until_parked();

        open_find(&view, cx, "body", "x");
        cx.update(|window, app| {
            view.update(app, |view, cx| view.toggle_document_replace(window, cx));
        });
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert!(view.document_find.replace.expanded);
            assert!(view.document_find_input_is_focused());
            assert!(view.document_find.replace.input_focused);
            assert!(!view.document_find.input_focused);
        });

        cx.simulate_keystrokes("backspace");
        cx.simulate_keystrokes("secondary-z");
        cx.run_until_parked();
        assert_eq!(text(&view, cx), "body\n");
        assert!(!view.read_with(cx, |view, _| view.editor().can_undo()));
    }

    #[gpui::test]
    fn the_expanded_bar_reserves_two_rows_and_closing_resets_the_scope(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(crate::actions::register_key_bindings);
        let (view, cx) = cx.add_window_view(|_, cx| EditorView::new("foo\n", "Untitled", cx));
        cx.simulate_resize(gpui::size(px(960.0), px(760.0)));
        cx.run_until_parked();

        open_find(&view, cx, "foo", "bar");
        let collapsed = view.read_with(cx, |view, _| view.document_find_reserved_height());
        cx.update(|window, app| {
            view.update(app, |view, cx| view.toggle_document_replace(window, cx));
        });
        cx.run_until_parked();
        let expanded = view.read_with(cx, |view, _| view.document_find_reserved_height());
        assert_eq!(expanded, collapsed * 2.0);
        let bounds = cx.debug_bounds("document-find-bar").expect("bar painted");
        assert_eq!(bounds.size.height, px(expanded));
        assert!(cx.debug_bounds("document-replace-all").is_some());

        view.update(cx, |view, cx| view.toggle_document_replace_scope(cx));
        cx.update(|window, app| {
            view.update(app, |view, cx| {
                view.leave_document_find(window, cx);
            });
        });
        view.read_with(cx, |view, _| {
            assert_eq!(
                view.document_find.replace.scope_mode,
                ReplaceScopeMode::Document
            );
            assert!(view.document_find.replace.scope.is_none());
        });
    }

    #[test]
    fn index_after_replacement_skips_the_inserted_text_and_wraps_conservatively() {
        let matches = [
            SourceRange::new(0, 1),
            SourceRange::new(5, 6),
            SourceRange::new(9, 10),
        ];
        // Inserted text ends at 3: next match starting at or after it.
        assert_eq!(
            index_after_replacement(&matches, SourceRange::new(1, 3)),
            Some(1)
        );
        // A deletion that closed up at 5 selects the match now starting there.
        assert_eq!(
            index_after_replacement(&matches, SourceRange::empty(5)),
            Some(1)
        );
        // Nothing at or after: wrap to the first match only if it is entirely
        // before the inserted text.
        assert_eq!(
            index_after_replacement(&matches, SourceRange::new(10, 12)),
            Some(0)
        );
        assert_eq!(
            index_after_replacement(&[SourceRange::new(3, 4)], SourceRange::new(2, 5)),
            None
        );
        assert_eq!(index_after_replacement(&[], SourceRange::empty(0)), None);
    }
}
