//! Selection AI popup (Issue #449): the one-shot "選択した文章に指示する" screen
//! opened over the active document, from capturing the selection through
//! send, result review, and replace/regenerate/discard.
//!
//! Per `docs/ai-selection-actions-implementation-spec.md` sections 3-6:
//! - The popup captures one [`SelectionTarget`] (session, document instance,
//!   revision, and byte `SourceRange`) plus a snapshot of the selected text
//!   when it opens. Every send and every apply re-validates that capture
//!   against the live document before doing anything (`ai_selection_start`,
//!   `ai_selection_replace`, `ai_selection_regenerate`), and any edit observed
//!   while the popup is open invalidates it outright, even for an edit
//!   outside the captured range (`EditorView::after_input`) — this initial
//!   version does not attempt a narrower reconciliation.
//! - The popup never holds the application-wide `AiSnapshot`'s busy/ownership
//!   state itself; it only ever reads a fresh [`hane_ai::AiSnapshot`] via
//!   `AiServiceHandle::snapshot()` to decide whether sending is currently
//!   possible, and receives its own request's result on that request's own
//!   dedicated channel (`AiServiceHandle::try_submit_text_transform`), never
//!   through the shared snapshot.
//! - `replace_range_recorded` is the only way the popup ever touches the
//!   document, and only after the apply-time re-validation above passes.

use super::*;
use gpui::{AppContext, Entity};
use gpui_component::input::{Input, InputEvent, InputState};
use hane_ai::{
    AccountState, ActiveConnection, AdmissionError, AiServiceHandle, AiSnapshot, OperationId,
    OwnershipState, TextTransformErrorCode, TextTransformOutcome, UserPrompt, UserPrompts,
    UserPromptsLoadError,
};
use hane_session::FileStateStore;
use std::sync::mpsc::RecvTimeoutError;

/// Shown whenever a send or an apply discovers the captured selection is no
/// longer valid (the document was edited, reloaded, or replaced since the
/// popup opened). The popup always closes itself when this fires.
pub(super) const AI_SELECTION_STALE_MESSAGE: &str =
    "文章が変更されたため、この結果は適用できません。選択し直して実行してください。";
const AI_SELECTION_NO_SELECTION_MESSAGE: &str = "AIで処理する文章を選択してください";
/// A generous upper bound on top of the service's own
/// `text_transform::TEXT_TRANSFORM_TIMEOUT`, purely so this background thread
/// cannot block forever if the service were ever to stop replying at all.
const AI_SELECTION_RECEIVE_TIMEOUT: Duration = Duration::from_secs(120);

/// One popup invocation's fixed target, captured once when it opens (section
/// 4). `instance`+`revision` together detect both "a different document
/// instance now occupies this id" and "the same instance was edited since".
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SelectionTarget {
    session: SessionId,
    instance: DocumentInstance,
    revision: Revision,
    range: SourceRange,
}

/// The one in-flight text-transform request, if any. `generation` (not just
/// `operation_id`) guards against a reply for an older request landing after
/// a newer one has already started — see `AiSelectionState::apply_outcome`.
struct PendingRequest {
    operation_id: OperationId,
    generation: u64,
}

#[derive(Clone, Debug, Default)]
enum AiSelectionPhase {
    #[default]
    Composing,
    Submitting,
    Reviewing {
        result: String,
        unchanged: bool,
    },
}

#[derive(Default)]
pub(super) struct AiSelectionState {
    service: Option<AiServiceHandle>,
    open: bool,
    target: Option<SelectionTarget>,
    original_text: String,
    confirmed_instruction: String,
    input: Option<Entity<InputState>>,
    input_subscription: Option<Subscription>,
    input_focused: bool,
    saved_prompts: Vec<UserPrompt>,
    prompts_load_generation: u64,
    phase: AiSelectionPhase,
    pending: Option<PendingRequest>,
    request_generation: u64,
    message: Option<String>,
    escape_was_composing: bool,
    enter_was_composing: bool,
}

impl AiSelectionState {
    pub(super) fn attach(&mut self, service: Option<AiServiceHandle>) {
        self.service = service;
    }

    /// Clears every field back to its closed default, best-effort requesting
    /// cancellation of any in-flight request first. Safe to call whether or
    /// not the popup is currently open.
    pub(super) fn reset(&mut self) {
        self.cancel_pending();
        self.open = false;
        self.target = None;
        self.original_text.clear();
        self.confirmed_instruction.clear();
        self.input = None;
        self.input_subscription = None;
        self.input_focused = false;
        self.phase = AiSelectionPhase::Composing;
        self.message = None;
        self.saved_prompts = Vec::new();
        self.escape_was_composing = false;
        self.enter_was_composing = false;
    }

    /// `reset`, but only (and reporting `true`) if the popup was actually
    /// open — so `EditorView::after_input` can tell "this edit just
    /// invalidated a visible popup" apart from "the popup was already
    /// closed" before deciding whether to surface a status message.
    pub(super) fn invalidate_for_edit(&mut self) -> bool {
        if !self.open {
            return false;
        }
        self.reset();
        true
    }

    fn cancel_pending(&mut self) {
        if let Some(pending) = self.pending.take()
            && let Some(service) = &self.service
        {
            let _ = service.cancel(pending.operation_id);
        }
    }

    fn apply_saved_prompts(
        &mut self,
        generation: u64,
        outcome: Result<Option<UserPrompts>, UserPromptsLoadError>,
    ) {
        if generation != self.prompts_load_generation {
            return;
        }
        self.saved_prompts = match outcome {
            Ok(Some(prompts)) => prompts.prompts,
            // A load failure must not pretend saved data exists; showing an
            // empty "未登録" list (rather than, say, retrying with a default)
            // is the honest answer for this read-only menu.
            Ok(None) | Err(_) => Vec::new(),
        };
    }

    /// Admits one text-transform request through the shared service and
    /// arms a background wait for its dedicated reply channel. Callers are
    /// responsible for re-validating `target`/`original_text` freshness
    /// first (see `EditorView::ai_selection_target_is_fresh`).
    fn start_request(&mut self, instruction: String, cx: &mut Context<EditorView>) {
        self.message = None;
        let Some(service) = self.service.clone() else {
            self.message = Some("AIサービスを利用できません。".to_owned());
            return;
        };
        let selected_text = self.original_text.clone();
        match service.try_submit_text_transform(instruction, selected_text) {
            Ok((operation_id, receiver)) => {
                self.request_generation = self.request_generation.wrapping_add(1);
                let generation = self.request_generation;
                self.pending = Some(PendingRequest {
                    operation_id,
                    generation,
                });
                self.phase = AiSelectionPhase::Submitting;
                let view = cx.entity().downgrade();
                cx.spawn(async move |_, cx| {
                    let outcome = cx
                        .background_executor()
                        .spawn(async move { receiver.recv_timeout(AI_SELECTION_RECEIVE_TIMEOUT) })
                        .await;
                    let _ = view.update(cx, move |view, cx| {
                        view.ai_selection.apply_outcome(generation, operation_id, outcome);
                        cx.notify();
                    });
                })
                .detach();
            }
            Err(error) => {
                self.message = Some(text_transform_admission_message(error));
            }
        }
    }

    /// Applies one reply to `pending`, discarding it (without touching
    /// `phase`/`message`) unless it matches the exact `(generation,
    /// operation_id)` this instance is currently waiting on — covering a
    /// cancelled, superseded, or otherwise stale reply landing late, and a
    /// reply arriving after the popup itself has already been closed.
    fn apply_outcome(
        &mut self,
        generation: u64,
        operation_id: OperationId,
        outcome: Result<TextTransformOutcome, RecvTimeoutError>,
    ) {
        let matches_pending = self.pending.as_ref().is_some_and(|pending| {
            pending.generation == generation && pending.operation_id == operation_id
        });
        if !matches_pending {
            return;
        }
        self.pending = None;
        if !self.open {
            return;
        }
        match outcome {
            Ok(TextTransformOutcome::Succeeded { text }) => {
                let unchanged = text == self.original_text;
                self.phase = AiSelectionPhase::Reviewing {
                    result: text,
                    unchanged,
                };
            }
            Ok(TextTransformOutcome::Canceled) => {
                self.phase = AiSelectionPhase::Composing;
            }
            Ok(TextTransformOutcome::TimedOut) => {
                self.phase = AiSelectionPhase::Composing;
                self.message =
                    Some("応答が時間内に完了しませんでした。もう一度お試しください。".to_owned());
            }
            Ok(TextTransformOutcome::Failed(code)) => {
                self.phase = AiSelectionPhase::Composing;
                self.message = Some(text_transform_error_message(code));
            }
            Err(_) => {
                self.phase = AiSelectionPhase::Composing;
                self.message = Some("AIサービスからの応答を取得できませんでした。".to_owned());
            }
        }
    }
}

fn text_transform_connection_ready(snapshot: &AiSnapshot) -> bool {
    if snapshot.ownership != OwnershipState::Owned
        || snapshot.busy.is_some()
        || snapshot.recovery_required
    {
        return false;
    }
    match snapshot.settings.active_connection {
        ActiveConnection::ChatGpt => {
            snapshot
                .settings
                .chatgpt
                .model_id
                .as_deref()
                .is_some_and(|model| !model.trim().is_empty())
                && matches!(snapshot.account, AccountState::SignedIn { .. })
                && !snapshot.account_refresh_failed
        }
        ActiveConnection::Custom => snapshot.settings.custom.as_ref().is_some_and(|custom| {
            !custom.name.trim().is_empty()
                && !custom.base_url.trim().is_empty()
                && !custom.model_id.trim().is_empty()
                && custom.credential_ref.is_some()
        }),
    }
}

fn text_transform_status_line(snapshot: Option<&AiSnapshot>) -> String {
    let Some(snapshot) = snapshot else {
        return "AI接続を設定してください".to_owned();
    };
    if !text_transform_connection_ready(snapshot) {
        return "AI接続を設定してください".to_owned();
    }
    match snapshot.settings.active_connection {
        ActiveConnection::ChatGpt => format!(
            "ChatGPT / {} に送信します",
            snapshot.settings.chatgpt.model_id.clone().unwrap_or_default()
        ),
        ActiveConnection::Custom => {
            let custom = snapshot.settings.custom.as_ref();
            format!(
                "{} / {} に送信します",
                custom.map_or("", |custom| custom.name.as_str()),
                custom.map_or("", |custom| custom.model_id.as_str())
            )
        }
    }
}

fn text_transform_error_message(code: TextTransformErrorCode) -> String {
    match code {
        TextTransformErrorCode::InstructionEmpty => "指示を入力してください。".to_owned(),
        TextTransformErrorCode::InstructionTooLarge => {
            "指示が長すぎます。短くしてお試しください。".to_owned()
        }
        TextTransformErrorCode::SelectedTextEmpty => "選択範囲を確認してください。".to_owned(),
        TextTransformErrorCode::SelectedTextTooLarge => {
            "選択範囲が大きすぎます。範囲を絞ってお試しください。".to_owned()
        }
        TextTransformErrorCode::InvalidConfiguration => "AI設定を確認してください。".to_owned(),
        TextTransformErrorCode::AccountUnavailable => {
            "ChatGPTアカウント状態を取得できません。再ログインをお試しください。".to_owned()
        }
        TextTransformErrorCode::ModelUnavailable => {
            "モデルを利用できません。モデルIDを確認してください。".to_owned()
        }
        TextTransformErrorCode::CredentialUnavailable => {
            "APIキーを取得できません。登録状態を確認してください。".to_owned()
        }
        TextTransformErrorCode::OwnedElsewhere => {
            "別のHaneプロセスがAI runtimeを使用しています。".to_owned()
        }
        TextTransformErrorCode::Busy => {
            "別のAI処理が進行中です。完了後にもう一度お試しください。".to_owned()
        }
        TextTransformErrorCode::RuntimeUnavailable => {
            "AI runtimeを利用できません。AI設定から再起動してください。".to_owned()
        }
        TextTransformErrorCode::StaleGeneration => {
            "設定が更新されています。最新の状態で再試行してください。".to_owned()
        }
        TextTransformErrorCode::Unauthorized | TextTransformErrorCode::Forbidden => {
            "認証に失敗しました。接続設定を確認してください。".to_owned()
        }
        TextTransformErrorCode::RateLimited => {
            "利用制限に達しました。時間をおいて再試行してください。".to_owned()
        }
        TextTransformErrorCode::Network => {
            "接続に失敗しました。ネットワークと接続先を確認してください。".to_owned()
        }
        TextTransformErrorCode::TimedOut => {
            "時間内に応答しませんでした。もう一度お試しください。".to_owned()
        }
        TextTransformErrorCode::ProtocolMismatch => {
            "AIサービスの応答形式を確認できませんでした。".to_owned()
        }
        TextTransformErrorCode::NotificationOverflow => {
            "処理結果の通知を確認できません。成功とは扱っていません。".to_owned()
        }
        TextTransformErrorCode::SafetyProfileUnsupported => {
            "安全に実行できないため処理を止めました。".to_owned()
        }
        TextTransformErrorCode::ProviderFailed => "AIサービスが応答できませんでした。".to_owned(),
        TextTransformErrorCode::Canceled => "処理を取り消しました。".to_owned(),
        TextTransformErrorCode::Isolated => {
            "子プロセスの停止を確認できず、AI操作を停止しています。".to_owned()
        }
        TextTransformErrorCode::ContextLengthExceeded => {
            "文章が長すぎて処理できませんでした。範囲を絞ってお試しください。".to_owned()
        }
        TextTransformErrorCode::ResultEmpty => "AIから有効な結果を得られませんでした。".to_owned(),
        TextTransformErrorCode::ResultTooLarge => {
            "AIの結果が大きすぎるため適用できません。".to_owned()
        }
    }
}

fn text_transform_admission_message(error: AdmissionError) -> String {
    match error {
        AdmissionError::Busy => {
            "別のAI処理が進行中です。完了後にもう一度お試しください。".to_owned()
        }
        AdmissionError::QueueFull => {
            "AI処理を受け付けられませんでした。少し待って再試行してください。".to_owned()
        }
        AdmissionError::Unavailable => {
            "AI runtimeを利用できません。設定と実行環境を確認してください。".to_owned()
        }
        AdmissionError::WrongOperation => "この操作は現在のAI処理に適用できません。".to_owned(),
        AdmissionError::InvalidInput => "指示または選択範囲を確認してください。".to_owned(),
    }
}

impl EditorView {
    /// Whether the popup's own free-text input currently holds keyboard
    /// focus. `actions.rs` checks this before every body-editing key
    /// (Backspace, arrows, Undo/Redo, Copy/Cut/Paste, Tab, Enter, …) the same
    /// way it already checks `document_find_input_is_focused()`, so typing in
    /// this field never leaks into the document underneath it.
    pub(crate) fn ai_selection_input_is_focused(&self) -> bool {
        self.ai_selection.open && self.ai_selection.input_focused
    }

    pub(crate) fn ai_selection_should_leave_on_escape(&self) -> bool {
        self.ai_selection.open && !self.ai_selection.escape_was_composing
    }

    pub(crate) fn leave_ai_selection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.close_ai_selection(window, cx);
    }

    /// Closes the popup (if open), cancelling any in-flight request and
    /// returning keyboard focus to the document body. Never touches the
    /// document's text, selection history, or dirty state.
    pub(crate) fn close_ai_selection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.ai_selection.open {
            return;
        }
        self.ai_selection.reset();
        window.focus(&self.focus_handle, cx);
        cx.notify();
    }

    fn ai_selection_reject_as_stale(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.status = Some(AI_SELECTION_STALE_MESSAGE.to_owned());
        self.close_ai_selection(window, cx);
    }

    /// The single freshness check every send and apply runs first (section
    /// 4): the captured session is still the active one, still names the
    /// same document instance, has not been edited since (`revision`), and
    /// is not mid-IME-composition.
    fn ai_selection_target_is_fresh(&self, target: SelectionTarget) -> bool {
        self.sessions.active_id() == target.session
            && self.document_instance(target.session) == Some(target.instance)
            && self.sessions.get(target.session).is_some_and(|session| {
                session.revision() == target.revision && session.editor().ime().is_none()
            })
    }

    fn ensure_ai_selection_input(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.ai_selection.input.is_some() {
            return;
        }
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("どのように直しますか?"));
        let subscription = cx.subscribe(&input, |view, input, event: &InputEvent, cx| match event
        {
            InputEvent::Focus => {
                view.ai_selection.input_focused = true;
                cx.notify();
            }
            InputEvent::Blur => {
                view.ai_selection.input_focused = false;
                cx.notify();
            }
            InputEvent::Change => {}
            // Enter submits the free-text instruction unless it only
            // confirmed an in-progress IME composition (`enter_was_composing`,
            // snapshotted by `note_ai_selection_enter_keystroke` before this
            // event fires) or arrived as Shift+Enter, which this field has no
            // multi-line use for.
            InputEvent::PressEnter { shift, .. } => {
                if *shift || view.ai_selection.enter_was_composing {
                    return;
                }
                let text = input.read(cx).value().trim().to_owned();
                if !text.is_empty() {
                    view.ai_selection_start(text, cx);
                }
            }
        });
        self.ai_selection.input = Some(input);
        self.ai_selection.input_subscription = Some(subscription);
    }

    /// Snapshots whether the popup's input currently has an active IME
    /// marked range, for every Escape keystroke while it is open — mirrors
    /// `note_document_find_escape_keystroke`'s reasoning: the interceptor
    /// that calls this runs before any binding for the keystroke dispatches,
    /// so it is the only point that still observes the composition the
    /// pinned input widget is about to unmark on its own.
    pub(super) fn note_ai_selection_escape_keystroke(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.ai_selection.open || !self.ai_selection.input_focused {
            return;
        }
        let Some(input) = self.ai_selection.input.clone() else {
            return;
        };
        self.ai_selection.escape_was_composing = input.update(cx, |state, cx| {
            <InputState as gpui::EntityInputHandler>::marked_text_range(state, window, cx).is_some()
        });
    }

    /// Same reasoning as `note_ai_selection_escape_keystroke`, for Enter.
    pub(super) fn note_ai_selection_enter_keystroke(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.ai_selection.open || !self.ai_selection.input_focused {
            return;
        }
        let Some(input) = self.ai_selection.input.clone() else {
            return;
        };
        self.ai_selection.enter_was_composing = input.update(cx, |state, cx| {
            <InputState as gpui::EntityInputHandler>::marked_text_range(state, window, cx).is_some()
        });
    }

    /// Opens the popup over the active document's current selection
    /// (section 3.1/4). A no-op while settings are open, while the popup is
    /// already open, while another text field holds focus, while the body
    /// has an active IME composition, or when the selection is empty or
    /// whitespace-only — the last two show a one-shot status message instead
    /// of opening (per the implementation spec, this is deliberately a
    /// status message rather than the popup itself, since there is nothing
    /// yet to compose an instruction against).
    pub(crate) fn open_ai_selection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.settings_open() || self.ai_selection.open {
            return;
        }
        if self.inline_rename_active()
            || self.sidebar_filter_is_focused()
            || self.document_find_input_is_focused()
        {
            return;
        }
        if self.editor().ime().is_some() {
            return;
        }
        let session_id = self.sessions.active_id();
        let Some(session) = self.sessions.get(session_id) else {
            return;
        };
        let range = session.editor().selection().range();
        let selected_text = session.editor().selected_text();
        let revision = session.revision();
        if range.start == range.end {
            self.status = Some(AI_SELECTION_NO_SELECTION_MESSAGE.to_owned());
            cx.notify();
            return;
        }
        let Ok(text) = selected_text else {
            return;
        };
        if text.trim().is_empty() {
            self.status = Some(AI_SELECTION_NO_SELECTION_MESSAGE.to_owned());
            cx.notify();
            return;
        }
        let Some(instance) = self.document_instance(session_id) else {
            return;
        };
        self.ai_selection.target = Some(SelectionTarget {
            session: session_id,
            instance,
            revision,
            range,
        });
        self.ai_selection.original_text = text;
        self.ai_selection.confirmed_instruction.clear();
        self.ai_selection.message = None;
        self.ai_selection.phase = AiSelectionPhase::Composing;
        self.ai_selection.open = true;
        self.status = None;
        self.ensure_ai_selection_input(window, cx);
        if let Some(input) = self.ai_selection.input.clone() {
            input.update(cx, |state, cx| {
                state.set_value("", window, cx);
                state.focus(window, cx);
            });
        }
        self.reload_ai_selection_saved_prompts(cx);
        cx.notify();
    }

    /// Re-validates the captured target, then admits `instruction` (either
    /// the free-text field's value or a saved prompt's body) as one
    /// text-transform request.
    fn ai_selection_start(&mut self, instruction: String, cx: &mut Context<Self>) {
        let Some(target) = self.ai_selection.target else {
            return;
        };
        if !self.ai_selection_target_is_fresh(target) {
            self.status = Some(AI_SELECTION_STALE_MESSAGE.to_owned());
            self.ai_selection.reset();
            cx.notify();
            return;
        }
        self.ai_selection.confirmed_instruction = instruction.clone();
        self.ai_selection.start_request(instruction, cx);
        cx.notify();
    }

    fn ai_selection_submit_free_text(&mut self, cx: &mut Context<Self>) {
        let Some(input) = self.ai_selection.input.clone() else {
            return;
        };
        let text = input.read(cx).value().trim().to_owned();
        if text.is_empty() {
            return;
        }
        self.ai_selection_start(text, cx);
    }

    /// A saved prompt's title was clicked/activated: its body is sent
    /// immediately, never combined with whatever is in the free-text field.
    fn ai_selection_run_saved_prompt(&mut self, prompt: String, cx: &mut Context<Self>) {
        let prompt = prompt.trim().to_owned();
        if prompt.is_empty() {
            return;
        }
        self.ai_selection_start(prompt, cx);
    }

    /// Cancels the in-flight request and returns to the compose phase,
    /// without closing the popup.
    fn ai_selection_stop(&mut self, cx: &mut Context<Self>) {
        self.ai_selection.cancel_pending();
        self.ai_selection.phase = AiSelectionPhase::Composing;
        cx.notify();
    }

    /// "置き換える": re-validates the target, confirms the range's current
    /// text still matches exactly what was sent, then applies the result as
    /// one `replace_range_recorded` transaction and runs it through the
    /// normal post-edit pipeline (`after_input`) exactly once. A no-op for an
    /// unchanged result (section 6): nothing is recorded, so no dirty/history
    /// entry is added for a replacement that would not change anything.
    fn ai_selection_replace(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let AiSelectionPhase::Reviewing { result, unchanged } = self.ai_selection.phase.clone()
        else {
            return;
        };
        if unchanged {
            return;
        }
        let Some(target) = self.ai_selection.target else {
            return;
        };
        if !self.ai_selection_target_is_fresh(target) {
            self.ai_selection_reject_as_stale(window, cx);
            return;
        }
        let Some(session) = self.sessions.get(target.session) else {
            self.ai_selection_reject_as_stale(window, cx);
            return;
        };
        let Ok(current_text) = session.editor().document().text(target.range) else {
            self.ai_selection_reject_as_stale(window, cx);
            return;
        };
        if current_text != self.ai_selection.original_text {
            self.ai_selection_reject_as_stale(window, cx);
            return;
        }
        let selection_after = Selection {
            anchor: target.range.start,
            active: SourceOffset(target.range.start.0 + result.len()),
        };
        let Some(session) = self.sessions.get_mut(target.session) else {
            return;
        };
        match session
            .editor_mut()
            .replace_range_recorded(target.range, &result, selection_after)
        {
            Ok(_) => {
                // Closes the popup (and so `invalidate_for_edit` below sees it
                // already closed) before `after_input` runs the edit through
                // its normal dirty/parse/autosave/find-resync pipeline.
                self.close_ai_selection(window, cx);
                self.after_input(cx);
            }
            Err(_) => {
                self.ai_selection.message = Some("置換に失敗しました。".to_owned());
                cx.notify();
            }
        }
    }

    /// "再生成": re-validates the target the same way `ai_selection_replace`
    /// does, then starts a fresh request with the same captured selection
    /// and the same confirmed instruction — never the previous AI result.
    fn ai_selection_regenerate(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !matches!(self.ai_selection.phase, AiSelectionPhase::Reviewing { .. }) {
            return;
        }
        let Some(target) = self.ai_selection.target else {
            return;
        };
        if !self.ai_selection_target_is_fresh(target) {
            self.ai_selection_reject_as_stale(window, cx);
            return;
        }
        let Some(session) = self.sessions.get(target.session) else {
            self.ai_selection_reject_as_stale(window, cx);
            return;
        };
        let Ok(current_text) = session.editor().document().text(target.range) else {
            self.ai_selection_reject_as_stale(window, cx);
            return;
        };
        if current_text != self.ai_selection.original_text {
            self.ai_selection_reject_as_stale(window, cx);
            return;
        }
        let instruction = self.ai_selection.confirmed_instruction.clone();
        self.ai_selection.start_request(instruction, cx);
        cx.notify();
    }

    /// Reloads the read-only saved-prompt menu from the same persisted file
    /// `ai_prompts.rs`'s settings page edits (`ai_prompts::store_for`). A
    /// stale/failed load never shows data that is not actually saved.
    fn reload_ai_selection_saved_prompts(&mut self, cx: &mut Context<Self>) {
        let Some(root) = FileStateStore::from_environment()
            .ok()
            .map(|store| store.root().to_path_buf())
        else {
            self.ai_selection.saved_prompts = Vec::new();
            return;
        };
        self.ai_selection.prompts_load_generation =
            self.ai_selection.prompts_load_generation.wrapping_add(1);
        let generation = self.ai_selection.prompts_load_generation;
        let view = cx.entity().downgrade();
        cx.spawn(async move |_, cx| {
            let outcome = cx
                .background_executor()
                .spawn(async move { ai_prompts::store_for(&root).load() })
                .await;
            let _ = view.update(cx, move |view, cx| {
                view.ai_selection.apply_saved_prompts(generation, outcome);
                cx.notify();
            });
        })
        .detach();
    }

    /// The popup's current window-space anchor: the active end of the
    /// captured selection's own screen position (`caret_geometry`, already
    /// content-area-relative), placed below it with room for an estimated
    /// popup height, or above it when that would not fit. `snap_to_window()`
    /// at the call site then keeps the popup horizontally on-screen. Falls
    /// back to a position near the top-left of the content area on the rare
    /// frame where no caret geometry has been resolved yet.
    fn ai_selection_overlay_position(&self) -> gpui::Point<Pixels> {
        const POPUP_HEIGHT_ESTIMATE: f32 = 280.0;
        const GAP: f32 = 6.0;
        let base_x = self.main_column_left + self.theme.line_horizontal_padding;
        let base_y = self.theme.header_height + self.document_find_reserved_height();
        let Some(geometry) = self.caret_geometry() else {
            return point(px(base_x), px(base_y));
        };
        let x = self.main_column_left + geometry.x;
        let caret_top = base_y + geometry.y;
        let caret_bottom = caret_top + geometry.height;
        let window_bottom = base_y + self.viewport_height;
        let y = if caret_bottom + GAP + POPUP_HEIGHT_ESTIMATE <= window_bottom {
            caret_bottom + GAP
        } else {
            (caret_top - GAP - POPUP_HEIGHT_ESTIMATE).max(self.theme.header_height)
        };
        point(px(x), px(y))
    }

    /// The popup's full overlay element, or `None` while it is closed. Called
    /// once per frame from the main render pass, after the file-tab context
    /// menu/close-confirmation overlays.
    pub(super) fn ai_selection_overlay(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        if !self.ai_selection.open {
            return None;
        }
        let position = self.ai_selection_overlay_position();
        Some(
            anchored()
                .position(position)
                .snap_to_window()
                .child(self.ai_selection_overlay_element(window, cx))
                .into_any_element(),
        )
    }

    fn ai_selection_overlay_element(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        self.ensure_ai_selection_input(window, cx);
        let phase = self.ai_selection.phase.clone();
        let content = match phase {
            AiSelectionPhase::Composing => self.ai_selection_compose_element(cx).into_any_element(),
            AiSelectionPhase::Submitting => {
                self.ai_selection_submitting_element(cx).into_any_element()
            }
            AiSelectionPhase::Reviewing { result, unchanged } => self
                .ai_selection_reviewing_element(result, unchanged, cx)
                .into_any_element(),
        };
        div()
            .id("ai-selection-popup")
            .debug_selector(|| "ai-selection-popup".to_owned())
            .occlude()
            .w(px(360.0))
            .max_h(px(320.0))
            .flex()
            .flex_col()
            .rounded_sm()
            .border_1()
            .border_color(rgb(self.theme.header_foreground))
            .bg(rgb(self.theme.code_background))
            .text_color(rgb(self.theme.foreground))
            .on_mouse_down_out(cx.listener(|view, _, window, cx| {
                view.close_ai_selection(window, cx);
            }))
            .child(content)
    }

    fn ai_selection_compose_element(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme;
        let snapshot = self
            .ai_selection
            .service
            .as_ref()
            .map(AiServiceHandle::snapshot);
        let ready = snapshot.as_ref().is_some_and(text_transform_connection_ready);
        let status_line = text_transform_status_line(snapshot.as_ref());
        let input = self.ai_selection.input.clone();
        let view = cx.entity();
        let close = Button::new("ai-selection-close")
            .label("×")
            .small()
            .ghost()
            .on_click(move |_, window, app| {
                view.update(app, |view, cx| view.close_ai_selection(window, cx));
            });
        let header = div()
            .flex()
            .items_center()
            .justify_between()
            .px_2()
            .py_1()
            .text_size(px(12.0))
            .font_weight(gpui::FontWeight::BOLD)
            .child("選択した文章に指示する")
            .child(close);
        let view = cx.entity();
        let send = Button::new("ai-selection-send")
            .label("送信")
            .small()
            .primary()
            .disabled(!ready)
            .on_click(move |_, _, app| {
                view.update(app, |view, cx| view.ai_selection_submit_free_text(cx));
            });
        let input_row = div()
            .flex()
            .items_center()
            .gap_2()
            .px_2()
            .pb_1()
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.0))
                    .children(input.map(|input| Input::new(&input).aria_label("ai instruction"))),
            )
            .child(send);
        let mut body = div().flex().flex_col().child(header).child(input_row);
        if let Some(message) = &self.ai_selection.message {
            body = body.child(
                div()
                    .id("ai-selection-message")
                    .px_2()
                    .pb_1()
                    .text_size(px(11.0))
                    .text_color(rgb(0xb42318))
                    .child(message.clone()),
            );
        }
        body.child(
            div()
                .px_2()
                .pb_1()
                .text_size(px(11.0))
                .text_color(rgb(theme.quote_foreground))
                .child(status_line),
        )
        .child(self.ai_selection_prompt_list_element(cx))
        .child(
            div()
                .px_2()
                .py_1()
                .text_size(px(10.0))
                .text_color(rgb(theme.quote_foreground))
                .child("選択範囲のみを、表示中の接続先・モデルに送信します"),
        )
    }

    fn ai_selection_prompt_list_element(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme;
        let mut list = div()
            .id("ai-selection-saved-prompts")
            .flex()
            .flex_col()
            .gap_1()
            .max_h(px(120.0))
            .overflow_y_scroll()
            .px_2()
            .pb_1();
        list = list.child(
            div()
                .text_size(px(10.0))
                .text_color(rgb(theme.quote_foreground))
                .child("保存した指示"),
        );
        if self.ai_selection.saved_prompts.is_empty() {
            list = list.child(
                div()
                    .id("ai-selection-saved-prompts-empty")
                    .text_size(px(11.0))
                    .text_color(rgb(theme.quote_foreground))
                    .child("未登録"),
            );
        } else {
            for (index, saved) in self.ai_selection.saved_prompts.iter().enumerate() {
                let title = if saved.title.trim().is_empty() {
                    "（タイトル未入力）".to_owned()
                } else {
                    saved.title.clone()
                };
                let prompt = saved.prompt.clone();
                let view = cx.entity();
                let row = div()
                    .id(("ai-selection-saved-prompt", index))
                    .px_2()
                    .py_1()
                    .rounded_sm()
                    .cursor_pointer()
                    .hover(|style| style.bg(rgb(theme.sidebar_active_background)))
                    .child(title)
                    .on_click(move |_, _, app| {
                        view.update(app, |view, cx| {
                            view.ai_selection_run_saved_prompt(prompt.clone(), cx);
                        });
                    });
                list = list.child(row);
            }
        }
        list
    }

    fn ai_selection_submitting_element(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme;
        let view = cx.entity();
        let stop = Button::new("ai-selection-stop")
            .label("停止")
            .small()
            .ghost()
            .on_click(move |_, _, app| {
                view.update(app, |view, cx| view.ai_selection_stop(cx));
            });
        div()
            .flex()
            .flex_col()
            .gap_2()
            .p_2()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child("AIに送信しています…")
                    .child(stop),
            )
            .child(
                div()
                    .text_size(px(11.0))
                    .text_color(rgb(theme.quote_foreground))
                    .child("選択範囲のみを、表示中の接続先・モデルに送信します"),
            )
    }

    fn ai_selection_reviewing_element(
        &mut self,
        result: String,
        unchanged: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = self.theme;
        let original = self.ai_selection.original_text.clone();
        let view = cx.entity();
        let close = Button::new("ai-selection-close-review")
            .label("×")
            .small()
            .ghost()
            .on_click(move |_, window, app| {
                view.update(app, |view, cx| view.close_ai_selection(window, cx));
            });
        let header = div()
            .flex()
            .items_center()
            .justify_between()
            .px_2()
            .py_1()
            .text_size(px(12.0))
            .font_weight(gpui::FontWeight::BOLD)
            .child("結果を確認")
            .child(close);
        let compare = div()
            .id("ai-selection-compare")
            .flex()
            .flex_col()
            .gap_1()
            .px_2()
            .max_h(px(160.0))
            .overflow_y_scroll()
            .child(
                div()
                    .text_size(px(10.0))
                    .text_color(rgb(theme.quote_foreground))
                    .child("元の文章"),
            )
            .child(div().text_size(px(12.0)).child(original))
            .child(
                div()
                    .text_size(px(10.0))
                    .text_color(rgb(theme.quote_foreground))
                    .child("AIの結果"),
            )
            .child(
                div()
                    .id("ai-selection-result-text")
                    .text_size(px(12.0))
                    .child(result.clone()),
            );
        let mut footer = div().flex().items_center().gap_2().px_2().py_1();
        if unchanged {
            footer = footer.child(
                div()
                    .id("ai-selection-unchanged")
                    .text_size(px(11.0))
                    .text_color(rgb(theme.quote_foreground))
                    .child("変更はありません"),
            );
        } else {
            let view = cx.entity();
            let replace = Button::new("ai-selection-replace")
                .label("置き換える")
                .small()
                .primary()
                .on_click(move |_, window, app| {
                    view.update(app, |view, cx| view.ai_selection_replace(window, cx));
                });
            footer = footer.child(replace);
        }
        let view = cx.entity();
        let regenerate = Button::new("ai-selection-regenerate")
            .label("再生成")
            .small()
            .on_click(move |_, window, app| {
                view.update(app, |view, cx| view.ai_selection_regenerate(window, cx));
            });
        let view = cx.entity();
        let discard = Button::new("ai-selection-discard")
            .label("破棄")
            .small()
            .ghost()
            .on_click(move |_, window, app| {
                view.update(app, |view, cx| view.close_ai_selection(window, cx));
            });
        footer = footer.child(regenerate).child(discard);
        let mut body = div().flex().flex_col().child(header).child(compare);
        if let Some(message) = &self.ai_selection.message {
            body = body.child(
                div()
                    .id("ai-selection-review-message")
                    .px_2()
                    .text_size(px(11.0))
                    .text_color(rgb(0xb42318))
                    .child(message.clone()),
            );
        }
        body.child(footer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pending(generation: u64, operation_id: u64) -> PendingRequest {
        PendingRequest {
            operation_id: OperationId(operation_id),
            generation,
        }
    }

    #[test]
    fn apply_outcome_ignores_a_reply_for_a_superseded_generation_or_operation() {
        let mut state = AiSelectionState {
            open: true,
            pending: Some(pending(2, 20)),
            phase: AiSelectionPhase::Submitting,
            ..Default::default()
        };
        // Wrong generation: a reply for an earlier request must not disturb
        // the newer one still pending.
        state.apply_outcome(
            1,
            OperationId(20),
            Ok(TextTransformOutcome::Succeeded {
                text: "stale".to_owned(),
            }),
        );
        assert!(matches!(state.phase, AiSelectionPhase::Submitting));
        assert!(state.pending.is_some());

        // Wrong operation id at the right generation: also ignored.
        state.apply_outcome(
            2,
            OperationId(99),
            Ok(TextTransformOutcome::Succeeded {
                text: "stale".to_owned(),
            }),
        );
        assert!(matches!(state.phase, AiSelectionPhase::Submitting));
        assert!(state.pending.is_some());
    }

    #[test]
    fn apply_outcome_is_dropped_once_the_popup_has_closed() {
        let mut state = AiSelectionState {
            open: false,
            pending: Some(pending(5, 50)),
            phase: AiSelectionPhase::Submitting,
            ..Default::default()
        };
        state.apply_outcome(
            5,
            OperationId(50),
            Ok(TextTransformOutcome::Succeeded {
                text: "late".to_owned(),
            }),
        );
        // The matching reply still clears `pending` (so a cancel is never
        // sent for an already-answered operation), but must not resurrect a
        // closed popup into the review phase.
        assert!(state.pending.is_none());
        assert!(matches!(state.phase, AiSelectionPhase::Submitting));
    }

    #[test]
    fn apply_outcome_detects_an_unchanged_result() {
        let mut state = AiSelectionState {
            open: true,
            pending: Some(pending(1, 1)),
            original_text: "元の文章".to_owned(),
            ..Default::default()
        };
        state.apply_outcome(
            1,
            OperationId(1),
            Ok(TextTransformOutcome::Succeeded {
                text: "元の文章".to_owned(),
            }),
        );
        match state.phase {
            AiSelectionPhase::Reviewing { result, unchanged } => {
                assert_eq!(result, "元の文章");
                assert!(unchanged);
            }
            other => panic!("expected Reviewing, got {other:?}"),
        }
    }

    #[test]
    fn apply_outcome_maps_failure_timeout_and_cancellation_back_to_composing() {
        let fresh = || AiSelectionState {
            open: true,
            pending: Some(pending(1, 1)),
            ..Default::default()
        };

        let mut state = fresh();
        state.apply_outcome(1, OperationId(1), Ok(TextTransformOutcome::Canceled));
        assert!(matches!(state.phase, AiSelectionPhase::Composing));
        assert!(state.message.is_none());

        let mut state = fresh();
        state.apply_outcome(1, OperationId(1), Ok(TextTransformOutcome::TimedOut));
        assert!(matches!(state.phase, AiSelectionPhase::Composing));
        assert!(state.message.is_some());

        let mut state = fresh();
        state.apply_outcome(
            1,
            OperationId(1),
            Ok(TextTransformOutcome::Failed(TextTransformErrorCode::Busy)),
        );
        assert!(matches!(state.phase, AiSelectionPhase::Composing));
        assert!(state.message.is_some());
    }

    #[gpui::test]
    fn opening_without_a_selection_shows_guidance_and_does_not_open(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(crate::actions::register_key_bindings);
        let (view, cx) = cx.add_window_view(|_, cx| EditorView::new("one two\n", "Untitled", cx));
        cx.simulate_resize(gpui::size(px(960.0), px(760.0)));
        cx.run_until_parked();

        cx.update(|window, app| {
            view.update(app, |view, cx| view.open_ai_selection(window, cx));
        });
        cx.run_until_parked();

        view.read_with(cx, |view, _| {
            assert!(!view.ai_selection.open);
            assert_eq!(view.status.as_deref(), Some(AI_SELECTION_NO_SELECTION_MESSAGE));
        });
    }

    #[gpui::test]
    fn opening_with_a_selection_captures_the_exact_selected_text(cx: &mut gpui::TestAppContext) {
        cx.update(crate::actions::register_key_bindings);
        let (view, cx) =
            cx.add_window_view(|_, cx| EditorView::new("日本語の文章です\n", "Untitled", cx));
        cx.simulate_resize(gpui::size(px(960.0), px(760.0)));
        cx.run_until_parked();

        cx.update(|window, app| {
            view.update(app, |view, cx| {
                view.editor_mut()
                    .set_selection(Selection {
                        anchor: SourceOffset(0),
                        active: SourceOffset("日本語".len()),
                    })
                    .unwrap();
                view.open_ai_selection(window, cx);
            });
        });
        cx.run_until_parked();

        view.read_with(cx, |view, _| {
            assert!(view.ai_selection.open);
            assert_eq!(view.ai_selection.original_text, "日本語");
            assert_eq!(view.editor().document().full_text(), "日本語の文章です\n");
        });
    }

    #[gpui::test]
    fn replace_applies_a_byte_accurate_multibyte_result_as_one_undoable_edit(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(crate::actions::register_key_bindings);
        let (view, cx) =
            cx.add_window_view(|_, cx| EditorView::new("日本語の文章です\n", "Untitled", cx));
        cx.simulate_resize(gpui::size(px(960.0), px(760.0)));
        cx.run_until_parked();

        cx.update(|window, app| {
            view.update(app, |view, cx| {
                view.editor_mut()
                    .set_selection(Selection {
                        anchor: SourceOffset(0),
                        active: SourceOffset("日本語".len()),
                    })
                    .unwrap();
                view.open_ai_selection(window, cx);
            });
        });
        cx.run_until_parked();

        view.update(cx, |view, cx| {
            view.ai_selection.phase = AiSelectionPhase::Reviewing {
                result: "改訂版".to_owned(),
                unchanged: false,
            };
            cx.notify();
        });

        cx.update(|window, app| {
            view.update(app, |view, cx| view.ai_selection_replace(window, cx));
        });
        cx.run_until_parked();

        view.read_with(cx, |view, _| {
            assert_eq!(view.editor().document().full_text(), "改訂版の文章です\n");
            assert!(!view.ai_selection.open);
            assert!(view.editor().can_undo());
            assert_eq!(
                view.editor().selection(),
                Selection {
                    anchor: SourceOffset(0),
                    active: SourceOffset("改訂版".len()),
                }
            );
        });

        view.update(cx, |view, cx| view.dispatch(EditorCommand::Undo, cx));
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert_eq!(view.editor().document().full_text(), "日本語の文章です\n");
        });

        view.update(cx, |view, cx| view.dispatch(EditorCommand::Redo, cx));
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert_eq!(view.editor().document().full_text(), "改訂版の文章です\n");
        });
    }

    #[gpui::test]
    fn replace_is_a_no_op_when_the_result_equals_the_original_text(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(crate::actions::register_key_bindings);
        let (view, cx) = cx.add_window_view(|_, cx| EditorView::new("same text\n", "Untitled", cx));
        cx.simulate_resize(gpui::size(px(960.0), px(760.0)));
        cx.run_until_parked();

        cx.update(|window, app| {
            view.update(app, |view, cx| {
                view.editor_mut()
                    .set_selection(Selection {
                        anchor: SourceOffset(0),
                        active: SourceOffset(4),
                    })
                    .unwrap();
                view.open_ai_selection(window, cx);
            });
        });
        cx.run_until_parked();

        view.update(cx, |view, cx| {
            view.ai_selection.phase = AiSelectionPhase::Reviewing {
                result: "same".to_owned(),
                unchanged: true,
            };
            cx.notify();
        });

        cx.update(|window, app| {
            view.update(app, |view, cx| view.ai_selection_replace(window, cx));
        });
        cx.run_until_parked();

        view.read_with(cx, |view, _| {
            assert_eq!(view.editor().document().full_text(), "same text\n");
            assert!(!view.editor().can_undo());
            // An unchanged result is a no-op, not an apply: the popup stays
            // open for the user to discard/regenerate explicitly.
            assert!(view.ai_selection.open);
        });
    }

    #[gpui::test]
    fn an_edit_after_opening_invalidates_the_pending_review_and_blocks_replace(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(crate::actions::register_key_bindings);
        let (view, cx) = cx.add_window_view(|_, cx| EditorView::new("abcdef\n", "Untitled", cx));
        cx.simulate_resize(gpui::size(px(960.0), px(760.0)));
        cx.run_until_parked();

        cx.update(|window, app| {
            view.update(app, |view, cx| {
                view.editor_mut()
                    .set_selection(Selection {
                        anchor: SourceOffset(0),
                        active: SourceOffset(3),
                    })
                    .unwrap();
                view.open_ai_selection(window, cx);
            });
        });
        cx.run_until_parked();

        view.update(cx, |view, cx| {
            view.ai_selection.phase = AiSelectionPhase::Reviewing {
                result: "XYZ".to_owned(),
                unchanged: false,
            };
            cx.notify();
        });

        // An edit far outside the captured range still invalidates the
        // popup in this initial version (implementation spec section 4).
        view.update(cx, |view, cx| {
            view.editor_mut()
                .set_selection(Selection::caret(SourceOffset(6)))
                .unwrap();
            view.dispatch(EditorCommand::Insert("!"), cx);
        });
        cx.run_until_parked();

        view.read_with(cx, |view, _| {
            assert!(!view.ai_selection.open);
            assert_eq!(
                view.status.as_deref(),
                Some(AI_SELECTION_STALE_MESSAGE)
            );
            assert_eq!(view.editor().document().full_text(), "abcdef!\n");
        });
    }

    #[gpui::test]
    fn escape_discards_without_touching_the_document_or_its_selection(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(crate::actions::register_key_bindings);
        let (view, cx) = cx.add_window_view(|_, cx| EditorView::new("abcdef\n", "Untitled", cx));
        cx.simulate_resize(gpui::size(px(960.0), px(760.0)));
        cx.run_until_parked();

        cx.update(|window, app| {
            view.update(app, |view, cx| {
                view.editor_mut()
                    .set_selection(Selection {
                        anchor: SourceOffset(0),
                        active: SourceOffset(3),
                    })
                    .unwrap();
                view.open_ai_selection(window, cx);
            });
        });
        cx.run_until_parked();
        assert!(view.read_with(cx, |view, _| view.ai_selection.open));

        cx.update(|window, app| {
            view.update(app, |view, cx| view.leave_ai_selection(window, cx));
        });
        cx.run_until_parked();

        view.read_with(cx, |view, _| {
            assert!(!view.ai_selection.open);
            assert_eq!(view.editor().document().full_text(), "abcdef\n");
            assert!(!view.editor().can_undo());
        });
    }
}
