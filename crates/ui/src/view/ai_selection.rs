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
    UserPromptsLoadError, initial_default_draft,
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
/// The popup content area's own height cap, shared between
/// `ai_selection_overlay_element`'s `max_h` and the below/above fit check in
/// `ai_selection_overlay_position` so the two stay in sync.
const AI_SELECTION_POPUP_MAX_CONTENT_HEIGHT: f32 = 320.0;
/// The popup's width when the editor area is wide enough; a narrower area
/// shrinks the popup to fit instead (`ai_selection_popup_width`).
const AI_SELECTION_POPUP_WIDTH: f32 = 360.0;

/// The popup's width inside an editor area `area_width` wide: the regular
/// width, or the whole area when that is narrower.
fn ai_selection_popup_width_for(area_width: f32) -> f32 {
    AI_SELECTION_POPUP_WIDTH.min(area_width.max(0.0))
}

/// Whether a mouse press at `(x, y)` landed inside the window's client area
/// (`0..width` x `0..height`). A press on the OS window frame (resize drag)
/// can reach the popup's outside-click handler with a position on or beyond
/// the client edge; that is not an intentional click on the document, so it
/// must not close the popup or discard the unsent input.
fn ai_selection_press_is_inside_client(x: f32, y: f32, width: f32, height: f32) -> bool {
    x >= 0.0 && y >= 0.0 && x < width && y < height
}

/// Shifts the popup's left edge `x` so a popup `width` wide stays inside the
/// editor area spanning `area_left..area_left + area_width`.
fn ai_selection_clamp_left(x: f32, area_left: f32, area_width: f32, width: f32) -> f32 {
    x.min(area_left + area_width - width).max(area_left)
}

/// The built-in suggestion shown in the saved-prompt list only while nothing
/// has ever been saved (`UserPromptsStore::load` returned `Ok(None)`),
/// mirroring the settings page's own blank-state default
/// (`ai_prompts::AiPromptsState::apply_load`'s `Ok(None)` branch). `id: 0` is
/// never issued by `UserPromptsStore`, so this can never collide with a real
/// saved prompt; running it (`ai_selection_run_saved_prompt`) only ever sends
/// its `prompt` text and never persists it.
fn default_saved_prompt() -> UserPrompt {
    let draft = initial_default_draft();
    UserPrompt {
        id: 0,
        title: draft.title,
        prompt: draft.prompt,
    }
}

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

/// One popup screen's own Tab/Shift+Tab stop. `Input`/`Send`/`List`/`Close`
/// belong to the compose screen (implementation spec section 3.2): the
/// free-text input, the send button, the saved-prompt list, and the close
/// button — in that order. `Stop` belongs to the submitting screen, and
/// `Replace`/`Regenerate`/`Discard` belong to the reviewing screen (`Close`
/// is shared with it too, for its own "×"). Independent of `input_focused`,
/// which only tracks whether the pinned `InputState` widget currently holds
/// real GPUI keyboard focus; every stop other than `Input` instead parks
/// real focus on the shared `EditorView::focus_handle` (see
/// `ai_selection_set_focus_region`), the same technique
/// `focus_content_search_results` already uses for its own
/// keyboard-navigable list.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum AiSelectionFocusRegion {
    #[default]
    Input,
    Send,
    List,
    Close,
    Stop,
    Replace,
    Regenerate,
    Discard,
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
    /// Which stop of the current phase's own
    /// `ai_selection_current_focus_stops` currently has logical keyboard
    /// focus — the compose, submitting, or reviewing screen's own stops,
    /// depending on `phase`.
    focus_region: AiSelectionFocusRegion,
    /// The saved-prompt list's own keyboard-focused row, meaningful only
    /// while `focus_region` is `List`. Always kept within
    /// `saved_prompts`'s current bounds (or cleared if it becomes empty) —
    /// see `apply_saved_prompts` — so a reload or an item disappearing never
    /// leaves this pointed at an item that no longer exists.
    list_focus_index: Option<usize>,
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
        self.focus_region = AiSelectionFocusRegion::default();
        self.list_focus_index = None;
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
            // Nothing has ever been saved: show the same built-in suggestion
            // the settings page's own blank state shows
            // (`ai_prompts::AiPromptsState::apply_load`'s `Ok(None)` branch),
            // not an empty list — PR #450 review.
            Ok(None) => vec![default_saved_prompt()],
            // A load failure must not pretend saved data exists; showing an
            // empty "未登録" list (rather than, say, retrying with a default)
            // is the honest answer for this read-only menu.
            Err(_) => Vec::new(),
        };
        // A reload can shrink or empty the list out from under a keyboard
        // focus that was pointed into it; never leave `list_focus_index`
        // referencing an item that no longer exists, and fall back to the
        // next stop still guaranteed to exist (`Send`) if the list itself
        // disappeared while it held focus.
        if self.saved_prompts.is_empty() {
            self.list_focus_index = None;
            if self.focus_region == AiSelectionFocusRegion::List {
                self.focus_region = AiSelectionFocusRegion::Send;
            }
        } else if let Some(index) = self.list_focus_index {
            self.list_focus_index = Some(index.min(self.saved_prompts.len() - 1));
        }
    }

    /// Admits one text-transform request through the shared service and
    /// arms a background wait for its dedicated reply channel. Callers are
    /// responsible for re-validating `target`/`original_text` freshness
    /// first (see `EditorView::ai_selection_target_is_fresh`). This is the
    /// single chokepoint every send path funnels through (free-text Enter,
    /// the Send button, a saved-prompt row, and Regenerate), so re-checking
    /// `text_transform_connection_ready` against a fresh snapshot here —
    /// not only in the compose screen's own `ready`-gated Send button
    /// render — is what keeps every one of those paths from enqueueing a
    /// request while the connection is not actually ready (PR #450 review).
    fn start_request(&mut self, instruction: String, cx: &mut Context<EditorView>) {
        self.message = None;
        let Some(service) = self.service.clone() else {
            self.message = Some("AIサービスを利用できません。".to_owned());
            return;
        };
        if !text_transform_connection_ready(&service.snapshot()) {
            // Not an error: fall back to the compose screen's own existing
            // not-ready display (the disabled Send button and its status
            // line, both already recomputed from a fresh snapshot there)
            // rather than surfacing a redundant message here.
            self.phase = AiSelectionPhase::Composing;
            return;
        }
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
                // The submitting screen has exactly one stop (`Stop`); land
                // logical focus there up front so Tab/Enter reach it without
                // requiring a Tab first (`ai_selection_handle_tab`/
                // `ai_selection_handle_enter`), the same way `Reviewing`
                // below lands on its own first stop. Real keyboard focus is
                // synchronized off the (about to be unmounted) `Input`
                // widget separately, by `ai_selection_overlay_element` on
                // the very next render (PR #450 review) — this inner state
                // stays window/cx-agnostic so it keeps being usable from the
                // plain `#[test]`s below.
                self.focus_region = AiSelectionFocusRegion::Stop;
                self.list_focus_index = None;
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
                // Lands on the reviewing screen's own first stop up front —
                // `Replace` when offered, otherwise `Regenerate` — so Tab/
                // Enter reach it without requiring a Tab first, same as
                // `Submitting` above. Real keyboard focus is synchronized
                // off `Input` separately — see the comment in
                // `start_request`.
                self.focus_region = if unchanged {
                    AiSelectionFocusRegion::Regenerate
                } else {
                    AiSelectionFocusRegion::Replace
                };
                self.list_focus_index = None;
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
    /// Whether this popup currently owns keyboard input, for every one of
    /// its own focus stops (the free-text input, the send button, the
    /// saved-prompt list, and the close button — see
    /// `AiSelectionFocusRegion`), not only while its pinned input widget
    /// holds real GPUI focus. `actions.rs` checks this before every
    /// body-editing key (Backspace, arrows, Undo/Redo, Copy/Cut/Paste, Tab,
    /// Enter, …) the same way it already checks
    /// `document_find_input_is_focused()`, and `InputCapture::paint` checks
    /// it before registering the document body as the active text-input
    /// target — together, this is what keeps every one of those keys (not
    /// only ones typed while the free-text input specifically has focus)
    /// from leaking into the document underneath the popup, including while
    /// `Send`/`List`/`Close` has parked real focus on the shared
    /// `focus_handle` instead (see `ai_selection_set_focus_region`).
    pub(crate) fn ai_selection_blocks_editor_keys(&self) -> bool {
        self.ai_selection.open
    }

    /// The text the open popup was started on, for tests that click the real
    /// footer button and check what it captured.
    #[cfg(test)]
    pub(crate) fn ai_selection_original_text_for_test(&self) -> &str {
        &self.ai_selection.original_text
    }

    pub(crate) fn ai_selection_should_leave_on_escape(&self) -> bool {
        self.ai_selection.open && !self.ai_selection.escape_was_composing
    }

    /// The compose screen's Tab/Shift+Tab stops, in order (implementation
    /// spec section 3.2) — skipping `List` when there is nothing in it for
    /// arrow keys to move among.
    fn ai_selection_composing_focus_stops(&self) -> Vec<AiSelectionFocusRegion> {
        let mut stops = vec![AiSelectionFocusRegion::Input, AiSelectionFocusRegion::Send];
        if !self.ai_selection.saved_prompts.is_empty() {
            stops.push(AiSelectionFocusRegion::List);
        }
        stops.push(AiSelectionFocusRegion::Close);
        stops
    }

    /// The reviewing screen's Tab/Shift+Tab stops, in the same
    /// primary-action/secondary-actions/dismiss order as the compose
    /// screen's own stops above: 置き換える (skipped once the result is
    /// `unchanged`, since there is then nothing for it to apply — see
    /// `ai_selection_replace`), 再生成, 破棄, then × last.
    fn ai_selection_reviewing_focus_stops(&self, unchanged: bool) -> Vec<AiSelectionFocusRegion> {
        let mut stops = Vec::new();
        if !unchanged {
            stops.push(AiSelectionFocusRegion::Replace);
        }
        stops.push(AiSelectionFocusRegion::Regenerate);
        stops.push(AiSelectionFocusRegion::Discard);
        stops.push(AiSelectionFocusRegion::Close);
        stops
    }

    /// The current phase's own Tab/Shift+Tab stops: the compose screen's
    /// `Input`/`Send`/`List`/`Close`, the submitting screen's single `Stop`,
    /// or the reviewing screen's own stops above.
    fn ai_selection_current_focus_stops(&self) -> Vec<AiSelectionFocusRegion> {
        match &self.ai_selection.phase {
            AiSelectionPhase::Composing => self.ai_selection_composing_focus_stops(),
            AiSelectionPhase::Submitting => vec![AiSelectionFocusRegion::Stop],
            AiSelectionPhase::Reviewing { unchanged, .. } => {
                self.ai_selection_reviewing_focus_stops(*unchanged)
            }
        }
    }

    /// Moves logical focus to the next (`forward`) or previous stop in
    /// `ai_selection_current_focus_stops`, wrapping past either end.
    fn ai_selection_move_focus(&mut self, forward: bool, window: &mut Window, cx: &mut Context<Self>) {
        let stops = self.ai_selection_current_focus_stops();
        let current = stops
            .iter()
            .position(|region| *region == self.ai_selection.focus_region)
            .unwrap_or(0);
        let next = if forward {
            (current + 1) % stops.len()
        } else {
            (current + stops.len() - 1) % stops.len()
        };
        self.ai_selection_set_focus_region(stops[next], window, cx);
    }

    /// Applies one focus stop: `Input` returns real GPUI focus to the pinned
    /// free-text input (the only stop with a native focus target of its
    /// own), while `Send`/`List`/`Close` park it on the shared
    /// `focus_handle` instead — the same technique
    /// `focus_content_search_results` already uses for its own
    /// keyboard-navigable list, so Tab/arrow keystrokes keep reaching the
    /// root-level `actions.rs` handlers that interpret them against
    /// `focus_region` rather than the (now unfocused) input's own key
    /// bindings.
    fn ai_selection_set_focus_region(
        &mut self,
        region: AiSelectionFocusRegion,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.ai_selection.focus_region = region;
        self.ai_selection.list_focus_index =
            (region == AiSelectionFocusRegion::List).then_some(0);
        if region == AiSelectionFocusRegion::Input {
            if let Some(input) = self.ai_selection.input.clone() {
                input.update(cx, |state, cx| state.focus(window, cx));
            }
            // Set synchronously rather than waiting for the pinned input's
            // own `InputEvent::Focus` — see the same reasoning in
            // `open_ai_selection`, which applies equally to cycling Tab/
            // Shift+Tab back onto `Input` (e.g. wrapping past `Close`).
            self.ai_selection.input_focused = true;
        } else {
            window.focus(&self.focus_handle, cx);
            // Same reasoning as the `Input` branch above, symmetrically
            // applied: clear this synchronously rather than waiting for the
            // pinned input's own async `InputEvent::Blur`, whose effect-queue
            // delivery is not guaranteed to have been observed before the
            // very next keystroke (e.g. `note_ai_selection_escape_keystroke`
            // reading `input_focused` right after a Tab moved focus to
            // `Send`/`List`/`Close`). Leaving a stale `true` here would make
            // that Escape keep treating the input as focused and never close
            // the popup.
            self.ai_selection.input_focused = false;
        }
        cx.notify();
    }

    /// Moves the saved-prompt list's own keyboard-focused row, wrapping past
    /// either end. A no-op when the list is empty.
    fn ai_selection_move_list_focus(&mut self, down: bool, cx: &mut Context<Self>) {
        let len = self.ai_selection.saved_prompts.len();
        if len == 0 {
            return;
        }
        let current = self.ai_selection.list_focus_index.unwrap_or(0);
        let next = if down {
            (current + 1) % len
        } else {
            (current + len - 1) % len
        };
        self.ai_selection.list_focus_index = Some(next);
        cx.notify();
    }

    /// Tab/Shift+Tab while this popup is open (implementation spec section
    /// 3.2, extended to the submitting/reviewing screens): cycles the
    /// current phase's own focus stops (`ai_selection_current_focus_stops`)
    /// and reports `true` so `actions.rs` never falls through to the
    /// document's own Indent/Outdent — including while submitting or
    /// reviewing, where there is no document-editing meaning for Tab to
    /// fall through to anyway, and the key must still never leak into the
    /// document underneath.
    pub(crate) fn ai_selection_handle_tab(
        &mut self,
        forward: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.ai_selection.open {
            return false;
        }
        self.ai_selection_move_focus(forward, window, cx);
        true
    }

    /// Up/Down while this popup is open: moves the saved-prompt list's own
    /// keyboard-focused row only while that list itself currently holds
    /// logical focus (implementation spec section 3.2); otherwise swallows
    /// the key without moving anything, same as `ai_selection_handle_tab`.
    pub(crate) fn ai_selection_handle_vertical_arrow(
        &mut self,
        down: bool,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.ai_selection.open {
            return false;
        }
        if matches!(self.ai_selection.phase, AiSelectionPhase::Composing)
            && self.ai_selection.focus_region == AiSelectionFocusRegion::List
        {
            self.ai_selection_move_list_focus(down, cx);
        }
        true
    }

    /// Enter while this popup is open, dispatched against whichever stop of
    /// the current phase's own focus stops
    /// (`ai_selection_current_focus_stops`) currently holds logical focus.
    ///
    /// On the compose screen (implementation spec section 3.2), every stop
    /// except `Input` decides something; `Input` is deliberately a no-op
    /// here: while that stop holds real GPUI focus, the pinned `InputState`
    /// widget's own `InputEvent::PressEnter` subscription (wired in
    /// `ensure_ai_selection_input`) already submits the free-text
    /// instruction and already guards Shift+Enter and an in-progress IME
    /// composition — handling it again here would submit twice.
    ///
    /// On the submitting screen, Enter at its own `Stop` stop cancels the
    /// in-flight request the same as clicking "停止" (`ai_selection_stop`).
    /// On the reviewing screen, Enter decides the same action as clicking
    /// the correspondingly focused button: `Replace` → "置き換える"
    /// (`ai_selection_replace`, unreachable once the result is `unchanged`
    /// — see `ai_selection_reviewing_focus_stops`), `Regenerate` → "再生成"
    /// (`ai_selection_regenerate`), `Discard`/`Close` → close the popup the
    /// same as "破棄"/"×" (`close_ai_selection`).
    ///
    /// Every branch not matching the current phase's own stops (e.g. a
    /// leftover `focus_region` from a previous phase, see `ai_selection_stop`
    /// and `apply_outcome`'s Composing-returning branches) swallows the key
    /// without deciding anything, rather than falling back to some other
    /// phase's meaning for the same stop value.
    pub(crate) fn ai_selection_handle_enter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.ai_selection.phase.clone() {
            AiSelectionPhase::Composing => match self.ai_selection.focus_region {
                AiSelectionFocusRegion::Input => {}
                AiSelectionFocusRegion::Send => self.ai_selection_submit_free_text(cx),
                AiSelectionFocusRegion::List => {
                    if let Some(prompt) = self
                        .ai_selection
                        .list_focus_index
                        .and_then(|index| self.ai_selection.saved_prompts.get(index))
                        .map(|saved| saved.prompt.clone())
                    {
                        self.ai_selection_run_saved_prompt(prompt, cx);
                    }
                }
                AiSelectionFocusRegion::Close => self.close_ai_selection(window, cx),
                AiSelectionFocusRegion::Stop
                | AiSelectionFocusRegion::Replace
                | AiSelectionFocusRegion::Regenerate
                | AiSelectionFocusRegion::Discard => {}
            },
            AiSelectionPhase::Submitting => {
                if self.ai_selection.focus_region == AiSelectionFocusRegion::Stop {
                    self.ai_selection_stop(cx);
                }
            }
            AiSelectionPhase::Reviewing { unchanged, .. } => match self.ai_selection.focus_region {
                AiSelectionFocusRegion::Replace if !unchanged => {
                    self.ai_selection_replace(window, cx);
                }
                AiSelectionFocusRegion::Regenerate => self.ai_selection_regenerate(window, cx),
                AiSelectionFocusRegion::Discard | AiSelectionFocusRegion::Close => {
                    self.close_ai_selection(window, cx);
                }
                _ => {}
            },
        }
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
        let subscription = cx.subscribe_in(&input, window, |view, input, event: &InputEvent, window, cx| match event
        {
            InputEvent::Focus => {
                view.ai_selection.input_focused = true;
                // Pointer re-focus of the input must also bring the Tab
                // order's logical focus back to `Input`; otherwise Tab keeps
                // using a stale `focus_region` and skips Send. Ignore a late
                // Focus event if real focus has already moved elsewhere.
                if view.ai_selection.open
                    && matches!(view.ai_selection.phase, AiSelectionPhase::Composing)
                    && gpui::Focusable::focus_handle(input.read(cx), cx).is_focused(window)
                {
                    view.ai_selection.focus_region = AiSelectionFocusRegion::Input;
                    view.ai_selection.list_focus_index = None;
                }
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
    /// pinned input widget is about to unmark on its own. Once the input has
    /// been blurred (e.g. by Tab moving focus to Send/List/Close), there is
    /// no composition left to observe on it, so this resets
    /// `escape_was_composing` to `false` instead of leaving behind a stale
    /// `true` from the Escape that ended the composition — otherwise a later
    /// Escape, dispatched after the blur, would keep reading that stale
    /// flag and never close the popup.
    pub(super) fn note_ai_selection_escape_keystroke(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.ai_selection.open {
            return;
        }
        if !self.ai_selection.input_focused {
            self.ai_selection.escape_was_composing = false;
            return;
        }
        let Some(input) = self.ai_selection.input.clone() else {
            return;
        };
        self.ai_selection.escape_was_composing = input.update(cx, |state, cx| {
            <InputState as gpui::EntityInputHandler>::marked_text_range(state, window, cx).is_some()
        });
    }

    /// Same reasoning as `note_ai_selection_escape_keystroke`, for Enter —
    /// except this one does not also require `input_focused`. The pinned
    /// input's own `InputEvent::Focus`/`Blur` can still be mid-flight (not
    /// yet observed by this view) immediately after `open_ai_selection`
    /// hands it real focus, and reading a stale `false` here would let a
    /// composing Enter fall through to `InputEvent::PressEnter` as if it
    /// were a committed one, submitting the marked text as the instruction.
    /// Reading `marked_text_range` straight from the input is cheap and
    /// always current, and is harmlessly `None` whenever this input holds no
    /// composition (including while it does not have real focus at all).
    pub(super) fn note_ai_selection_enter_keystroke(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.ai_selection.open {
            return;
        }
        let Some(input) = self.ai_selection.input.clone() else {
            return;
        };
        self.ai_selection.enter_was_composing = input.update(cx, |state, cx| {
            <InputState as gpui::EntityInputHandler>::marked_text_range(state, window, cx).is_some()
        });
    }

    /// Whether `open_ai_selection` can start at all right now: not while
    /// settings or the popup are open, while another text field (rename,
    /// sidebar filter, document find) holds the input, or while the body has
    /// an active IME composition. The entry and the footer button share this
    /// one check so the button is disabled for exactly the states in which
    /// the entry would return without doing anything. It reads no selection
    /// text, so it is cheap enough to evaluate on every render.
    pub(crate) fn ai_selection_entry_available(&self) -> bool {
        if self.settings_open() || self.ai_selection.open {
            return false;
        }
        if self.inline_rename_active()
            || self.sidebar_filter_is_focused()
            || self.document_find_input_is_focused()
        {
            return false;
        }
        self.editor().ime().is_none()
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
        if !self.ai_selection_entry_available() {
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
        self.ai_selection.focus_region = AiSelectionFocusRegion::Input;
        self.ai_selection.list_focus_index = None;
        self.ai_selection.open = true;
        self.status = None;
        self.ensure_ai_selection_input(window, cx);
        if let Some(input) = self.ai_selection.input.clone() {
            input.update(cx, |state, cx| {
                state.set_value("", window, cx);
                state.focus(window, cx);
            });
        }
        // Set synchronously rather than waiting for the pinned input's own
        // `InputEvent::Focus` (see the `cx.subscribe` in
        // `ensure_ai_selection_input`): `note_ai_selection_escape_keystroke`
        // reads `input_focused` for the very next keystroke, and a Focus
        // event dispatched through GPUI's own effect queue is not guaranteed
        // to have been observed by then. `document_find`'s `open_document_find`
        // and `content_search`'s `open_content_search` set their own
        // `input_focused` the same way for the same reason.
        self.ai_selection.input_focused = true;
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
        if prompt.trim().is_empty() {
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

    /// Upper bound on the popup's own rendered height (its `max_h` in
    /// `ai_selection_overlay_element`, plus the 1px top/bottom border), used
    /// only to decide whether it fits *below* the selection without
    /// clipping. The final on-screen position never uses this bound: it
    /// comes from `Anchor::TopLeft`/`Anchor::BottomLeft` anchoring at the
    /// selection's own edge, which GPUI resolves against the popup's real
    /// measured size (see `ai_selection_overlay`). A short panel (e.g. an
    /// empty saved-prompt list) therefore never leaves an oversized gap
    /// above the selection, and a tall one is never placed where it would
    /// overflow the editor area.
    const AI_SELECTION_POPUP_MAX_HEIGHT: f32 = AI_SELECTION_POPUP_MAX_CONTENT_HEIGHT + 2.0;

    /// The horizontal extent of the editor area the popup must stay within:
    /// the text column plus its side paddings, which is the window width minus
    /// the sidebar.
    fn ai_selection_editor_area_width(&self) -> f32 {
        self.content_width + 2.0 * self.theme.line_horizontal_padding
    }

    /// The popup's anchor point and the corner of the popup that should sit
    /// at that point: the active end of the captured selection's own screen
    /// position (`caret_geometry`, already content-area-relative), below the
    /// selection when `AI_SELECTION_POPUP_MAX_HEIGHT` fits there, or above it
    /// otherwise. `ai_selection_overlay` lets GPUI resolve the opposite edge
    /// from the popup's real measured size, so the gap next to the selection
    /// always matches the actually rendered content. Falls back to
    /// `Anchor::TopLeft` near the top-left of the content area on the rare
    /// frame where no caret geometry has been resolved yet.
    fn ai_selection_overlay_position(&self) -> (gpui::Point<Pixels>, Anchor) {
        const GAP: f32 = 6.0;
        let area_width = self.ai_selection_editor_area_width();
        let width = ai_selection_popup_width_for(area_width);
        let clamp_x =
            |x: f32| ai_selection_clamp_left(x, self.main_column_left, area_width, width);
        let base_x = clamp_x(self.main_column_left + self.theme.line_horizontal_padding);
        let base_y = self.theme.header_height + self.document_find_reserved_height();
        let Some(geometry) = self.caret_geometry() else {
            return (point(px(base_x), px(base_y)), Anchor::TopLeft);
        };
        let x = clamp_x(self.main_column_left + geometry.x);
        let caret_top = base_y + geometry.y;
        let caret_bottom = caret_top + geometry.height;
        let window_bottom = base_y + self.viewport_height;
        if caret_bottom + GAP + Self::AI_SELECTION_POPUP_MAX_HEIGHT <= window_bottom {
            (point(px(x), px(caret_bottom + GAP)), Anchor::TopLeft)
        } else {
            (point(px(x), px(caret_top - GAP)), Anchor::BottomLeft)
        }
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
        let (position, corner) = self.ai_selection_overlay_position();
        // Keeps the popup within the editor's own display area (below the
        // header/find bar, above the footer) rather than the whole window,
        // per the implementation spec's "左右と高さをエディタ表示領域内に
        // 収める"; horizontal margins stay 0, matching the pre-existing
        // left/right clamp against the window edges.
        let editor_area_margin = Edges {
            top: px(self.theme.header_height + self.document_find_reserved_height()),
            right: px(0.0),
            bottom: px(self.theme.footer_height),
            left: px(0.0),
        };
        Some(
            anchored()
                .anchor(corner)
                .position(position)
                .snap_to_window_with_margin(editor_area_margin)
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
        // Every focus stop other than `Input` parks real GPUI keyboard focus
        // on the shared `focus_handle` instead (see
        // `ai_selection_set_focus_region`). Re-asserting that on every frame
        // this popup is open — rather than only at the moment `focus_region`
        // changes — is what keeps real focus from staying parked on the
        // pinned `Input` widget once a phase transition (`start_request`
        // landing on `Stop`, `apply_outcome` landing on `Replace`/
        // `Regenerate`) stops rendering `Input` at all: with real focus left
        // on a now-unmounted widget, Tab/Enter stop reaching this popup's
        // own actions entirely (PR #450 review).
        //
        // The exception is a pointer click that has just moved real focus
        // onto the input while the logical region is still stale: the frame
        // rendered right after that click would otherwise steal focus back
        // before `InputEvent::Focus` is delivered. Adopt the input's real
        // focus as the logical region instead.
        let input_really_focused = matches!(self.ai_selection.phase, AiSelectionPhase::Composing)
            && self
                .ai_selection
                .input
                .as_ref()
                .is_some_and(|input| gpui::Focusable::focus_handle(input.read(cx), cx).is_focused(window));
        if input_really_focused {
            self.ai_selection.focus_region = AiSelectionFocusRegion::Input;
            self.ai_selection.list_focus_index = None;
            self.ai_selection.input_focused = true;
        } else if self.ai_selection.focus_region != AiSelectionFocusRegion::Input {
            window.focus(&self.focus_handle, cx);
            self.ai_selection.input_focused = false;
        }
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
            .w(px(ai_selection_popup_width_for(
                self.ai_selection_editor_area_width(),
            )))
            .min_w(px(0.0))
            .max_w(px(ai_selection_popup_width_for(
                self.ai_selection_editor_area_width(),
            )))
            .max_h(px(AI_SELECTION_POPUP_MAX_CONTENT_HEIGHT))
            .flex()
            .flex_col()
            .rounded_sm()
            .border_1()
            .border_color(rgb(self.theme.header_foreground))
            .bg(rgb(self.theme.code_background))
            .text_color(rgb(self.theme.foreground))
            .on_mouse_down_out(cx.listener(|view, event: &gpui::MouseDownEvent, window, cx| {
                let size = window.viewport_size();
                if !ai_selection_press_is_inside_client(
                    f32::from(event.position.x),
                    f32::from(event.position.y),
                    f32::from(size.width),
                    f32::from(size.height),
                ) {
                    return;
                }
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
        // Send/Close have no native focus target of their own while they
        // hold the compose screen's logical keyboard focus (real GPUI focus
        // instead parks on the shared `focus_handle`, see
        // `ai_selection_set_focus_region`), so this border is their only
        // on-screen focus indicator — the same role `keyboard_focused`'s
        // background fill plays for the saved-prompt list's rows below.
        let close_focused = self.ai_selection.focus_region == AiSelectionFocusRegion::Close;
        let send_focused = self.ai_selection.focus_region == AiSelectionFocusRegion::Send;
        let view = cx.entity();
        let close = Button::new("ai-selection-close")
            .label("×")
            .small()
            .ghost()
            .on_click(move |_, window, app| {
                view.update(app, |view, cx| view.close_ai_selection(window, cx));
            });
        // The close control never shrinks and always keeps its slot at the
        // header's right edge; the title gives way (truncates) instead.
        let close = div()
            .id("ai-selection-close-focus-ring")
            .debug_selector(|| "ai-selection-close".to_owned())
            .flex_none()
            .rounded_sm()
            .when(close_focused, |el| {
                el.border_1().border_color(rgb(theme.link_foreground))
            })
            .child(close);
        let header = div()
            .flex()
            .items_center()
            .justify_between()
            .gap_2()
            .w_full()
            .min_w(px(0.0))
            .px_2()
            .py_1()
            .text_size(px(12.0))
            .font_weight(gpui::FontWeight::BOLD)
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.0))
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .child("選択した文章に指示する"),
            )
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
        let send = div()
            .id("ai-selection-send-focus-ring")
            .debug_selector(|| "ai-selection-send".to_owned())
            .flex_none()
            .rounded_sm()
            .when(send_focused, |el| {
                el.border_1().border_color(rgb(theme.link_foreground))
            })
            .child(send);
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
                    .debug_selector(|| "ai-selection-input".to_owned())
                    .children(input.map(|input| Input::new(&input).aria_label("ai instruction"))),
            )
            .child(send);
        let input_row = input_row.w_full().min_w(px(0.0));
        let mut body = div()
            .flex()
            .flex_col()
            .w_full()
            .min_w(px(0.0))
            .child(header)
            .child(input_row);
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
                let keyboard_focused = self.ai_selection.focus_region == AiSelectionFocusRegion::List
                    && self.ai_selection.list_focus_index == Some(index);
                let view = cx.entity();
                let row = div()
                    .id(("ai-selection-saved-prompt", index))
                    .px_2()
                    .py_1()
                    .rounded_sm()
                    .cursor_pointer()
                    .when(keyboard_focused, |el| {
                        el.bg(rgb(theme.sidebar_active_background))
                    })
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
        // `Stop` has no native focus target of its own while it holds this
        // screen's logical keyboard focus (real GPUI focus instead parks on
        // the shared `focus_handle`, see `ai_selection_set_focus_region`),
        // so this border is its only on-screen focus indicator — the same
        // role the compose screen's `send_focused`/`close_focused` borders
        // play for its own stops.
        let stop_focused = self.ai_selection.focus_region == AiSelectionFocusRegion::Stop;
        let stop = div()
            .id("ai-selection-stop-focus-ring")
            .rounded_sm()
            .when(stop_focused, |el| {
                el.border_1().border_color(rgb(theme.link_foreground))
            })
            .child(stop);
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
        // Each button below has no native focus target of its own while it
        // holds this screen's logical keyboard focus (real GPUI focus
        // instead parks on the shared `focus_handle`, see
        // `ai_selection_set_focus_region`), so its own border is its only
        // on-screen focus indicator — the same role the compose screen's
        // `send_focused`/`close_focused` borders play for its own stops.
        let close_focused = self.ai_selection.focus_region == AiSelectionFocusRegion::Close;
        let replace_focused = self.ai_selection.focus_region == AiSelectionFocusRegion::Replace;
        let regenerate_focused = self.ai_selection.focus_region == AiSelectionFocusRegion::Regenerate;
        let discard_focused = self.ai_selection.focus_region == AiSelectionFocusRegion::Discard;
        let view = cx.entity();
        let close = Button::new("ai-selection-close-review")
            .label("×")
            .small()
            .ghost()
            .on_click(move |_, window, app| {
                view.update(app, |view, cx| view.close_ai_selection(window, cx));
            });
        let close = div()
            .id("ai-selection-close-review-focus-ring")
            .rounded_sm()
            .when(close_focused, |el| {
                el.border_1().border_color(rgb(theme.link_foreground))
            })
            .child(close);
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
            let replace = div()
                .id("ai-selection-replace-focus-ring")
                .rounded_sm()
                .when(replace_focused, |el| {
                    el.border_1().border_color(rgb(theme.link_foreground))
                })
                .child(replace);
            footer = footer.child(replace);
        }
        let view = cx.entity();
        let regenerate = Button::new("ai-selection-regenerate")
            .label("再生成")
            .small()
            .on_click(move |_, window, app| {
                view.update(app, |view, cx| view.ai_selection_regenerate(window, cx));
            });
        let regenerate = div()
            .id("ai-selection-regenerate-focus-ring")
            .rounded_sm()
            .when(regenerate_focused, |el| {
                el.border_1().border_color(rgb(theme.link_foreground))
            })
            .child(regenerate);
        let view = cx.entity();
        let discard = Button::new("ai-selection-discard")
            .label("破棄")
            .small()
            .ghost()
            .on_click(move |_, window, app| {
                view.update(app, |view, cx| view.close_ai_selection(window, cx));
            });
        let discard = div()
            .id("ai-selection-discard-focus-ring")
            .rounded_sm()
            .when(discard_focused, |el| {
                el.border_1().border_color(rgb(theme.link_foreground))
            })
            .child(discard);
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

    // Issue #449 AC2 follow-up: the popup must stay close to the selection
    // on both the below and the flipped-above path, using the selection's
    // own edge rather than a fixed height guess (`POPUP_HEIGHT_ESTIMATE`)
    // that could overshoot the popup's real rendered height and leave a
    // large gap above the selection.
    #[gpui::test]
    fn overlay_position_sits_just_below_the_selection_when_the_editor_area_has_room(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(crate::actions::register_key_bindings);
        let (view, cx) = cx.add_window_view(|_, cx| EditorView::new("one two\n", "Untitled", cx));
        cx.simulate_resize(gpui::size(px(960.0), px(760.0)));
        cx.run_until_parked();

        view.update(cx, |view, _cx| {
            view.main_column_left = 0.0;
            view.viewport_height = 600.0;
            view.caret_geometry = Some(CaretGeometry {
                x: 100.0,
                y: 50.0,
                height: 20.0,
            });
        });

        view.read_with(cx, |view, _| {
            let (position, corner) = view.ai_selection_overlay_position();
            assert_eq!(corner, Anchor::TopLeft);
            let base_y = view.theme.header_height + view.document_find_reserved_height();
            // caret_bottom + GAP, independent of the popup's actual content.
            let expected_y = base_y + 50.0 + 20.0 + 6.0;
            assert!((f32::from(position.y) - expected_y).abs() < 0.01);
        });
    }

    #[gpui::test]
    fn overlay_position_flips_to_sit_just_above_the_selection_when_below_would_clip(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(crate::actions::register_key_bindings);
        let (view, cx) = cx.add_window_view(|_, cx| EditorView::new("one two\n", "Untitled", cx));
        cx.simulate_resize(gpui::size(px(960.0), px(760.0)));
        cx.run_until_parked();

        view.update(cx, |view, _cx| {
            view.main_column_left = 0.0;
            // Only ~60px of editor area left below the selection: far less
            // than the popup's own height cap, so this must flip above
            // instead of reusing a fixed height guess that overshoots
            // whatever the popup actually renders at (the reported bug: an
            // oversized, selection-distant gap for a short panel).
            view.viewport_height = 100.0;
            view.caret_geometry = Some(CaretGeometry {
                x: 100.0,
                y: 40.0,
                height: 20.0,
            });
        });

        view.read_with(cx, |view, _| {
            let (position, corner) = view.ai_selection_overlay_position();
            assert_eq!(corner, Anchor::BottomLeft);
            let base_y = view.theme.header_height + view.document_find_reserved_height();
            let caret_top = base_y + 40.0;
            // caret_top - GAP: GPUI resolves the popup's top edge from this
            // anchor using its real measured height (`Anchor::BottomLeft`),
            // so this position never needs — and never bakes in — any
            // estimate of that height.
            let expected_y = caret_top - 6.0;
            assert!((f32::from(position.y) - expected_y).abs() < 0.01);
        });
    }

    #[test]
    fn popup_width_keeps_regular_width_and_shrinks_to_narrow_areas() {
        assert_eq!(ai_selection_popup_width_for(960.0), 360.0);
        assert_eq!(ai_selection_popup_width_for(360.0), 360.0);
        assert_eq!(ai_selection_popup_width_for(280.0), 280.0);
        assert_eq!(ai_selection_popup_width_for(279.0), 279.0);
        assert_eq!(ai_selection_popup_width_for(-5.0), 0.0);
    }

    #[test]
    fn popup_left_edge_stays_inside_the_editor_area_at_every_width() {
        for area_width in [279.0_f32, 280.0, 960.0] {
            let width = ai_selection_popup_width_for(area_width);
            for (area_left, x) in [(0.0_f32, 0.0_f32), (0.0, 100.0), (0.0, 900.0), (200.0, 250.0)] {
                let left = ai_selection_clamp_left(x, area_left, area_width, width);
                assert!(left >= area_left, "left {left} at width {area_width}");
                assert!(
                    left + width <= area_left + area_width + 0.01,
                    "right edge {} at width {area_width}",
                    left + width
                );
            }
        }
        // Regular width keeps the anchor position when it already fits.
        assert_eq!(ai_selection_clamp_left(100.0, 0.0, 960.0, 360.0), 100.0);
    }

    #[test]
    fn frame_drag_presses_outside_the_client_area_do_not_count_as_outside_clicks() {
        for (width, height) in [(279.0_f32, 600.0_f32), (280.0, 600.0), (962.0, 600.0)] {
            // Intentional clicks inside the client area still close the popup.
            assert!(ai_selection_press_is_inside_client(10.0, 10.0, width, height));
            assert!(ai_selection_press_is_inside_client(width - 1.0, height - 1.0, width, height));
            // Frame presses (right/bottom/left/top border) are ignored.
            assert!(!ai_selection_press_is_inside_client(width, 100.0, width, height));
            assert!(!ai_selection_press_is_inside_client(width + 4.0, 100.0, width, height));
            assert!(!ai_selection_press_is_inside_client(100.0, height, width, height));
            assert!(!ai_selection_press_is_inside_client(-1.0, 100.0, width, height));
            assert!(!ai_selection_press_is_inside_client(100.0, -1.0, width, height));
        }
    }

    #[gpui::test]
    fn overlay_position_keeps_the_whole_popup_inside_a_narrow_editor_area(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(crate::actions::register_key_bindings);
        let (view, cx) = cx.add_window_view(|_, cx| EditorView::new("one two\n", "Untitled", cx));
        cx.simulate_resize(gpui::size(px(960.0), px(760.0)));
        cx.run_until_parked();

        for window_width in [279.0_f32, 280.0, 960.0] {
            view.update(cx, |view, _cx| {
                view.main_column_left = 0.0;
                view.content_width = window_width - 2.0 * view.theme.line_horizontal_padding;
                view.viewport_height = 600.0;
                view.caret_geometry = Some(CaretGeometry {
                    x: 250.0,
                    y: 50.0,
                    height: 20.0,
                });
            });
            view.read_with(cx, |view, _| {
                let (position, _) = view.ai_selection_overlay_position();
                let width = ai_selection_popup_width_for(view.ai_selection_editor_area_width());
                let left = f32::from(position.x);
                assert!(left >= 0.0);
                assert!(left + width <= window_width + 0.01);
            });
        }
    }

    #[gpui::test]
    fn close_and_send_controls_are_painted_inside_the_window_at_narrow_widths(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(crate::actions::register_key_bindings);
        let (view, cx) =
            cx.add_window_view(|_, cx| EditorView::new("one two three\n", "Untitled", cx));
        for window_width in [279.0_f32, 280.0, 960.0] {
            cx.simulate_resize(gpui::size(px(window_width), px(760.0)));
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
            for selector in ["ai-selection-popup", "ai-selection-close", "ai-selection-send"] {
                let bounds = cx
                    .debug_bounds(selector)
                    .unwrap_or_else(|| panic!("{selector} painted at width {window_width}"));
                assert!(
                    f32::from(bounds.left()) >= -0.01
                        && f32::from(bounds.right()) <= window_width + 0.01,
                    "{selector} {bounds:?} must stay inside width {window_width}"
                );
            }
            view.update(cx, |view, cx| {
                view.ai_selection.open = false;
                cx.notify();
            });
            cx.run_until_parked();
        }
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

    fn saved_prompt(id: u64, prompt: &str) -> UserPrompt {
        UserPrompt {
            id,
            title: format!("prompt-{id}"),
            prompt: prompt.to_owned(),
        }
    }

    #[gpui::test]
    fn tab_cycles_focus_through_input_send_list_and_close_and_shift_tab_reverses(
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
            view.ai_selection.saved_prompts = vec![saved_prompt(1, "直す")];
            cx.notify();
        });
        cx.run_until_parked();

        view.read_with(cx, |view, _| {
            assert_eq!(view.ai_selection.focus_region, AiSelectionFocusRegion::Input);
        });

        cx.simulate_keystrokes("tab");
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert_eq!(view.ai_selection.focus_region, AiSelectionFocusRegion::Send);
        });

        cx.simulate_keystrokes("tab");
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert_eq!(view.ai_selection.focus_region, AiSelectionFocusRegion::List);
            assert_eq!(view.ai_selection.list_focus_index, Some(0));
        });

        cx.simulate_keystrokes("tab");
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert_eq!(view.ai_selection.focus_region, AiSelectionFocusRegion::Close);
            // Leaving the list clears its own keyboard-focused row.
            assert_eq!(view.ai_selection.list_focus_index, None);
        });

        cx.simulate_keystrokes("tab");
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert_eq!(view.ai_selection.focus_region, AiSelectionFocusRegion::Input);
        });

        // Shift+Tab walks the exact same stops backward.
        cx.simulate_keystrokes("shift-tab");
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert_eq!(view.ai_selection.focus_region, AiSelectionFocusRegion::Close);
        });
        cx.simulate_keystrokes("shift-tab");
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert_eq!(view.ai_selection.focus_region, AiSelectionFocusRegion::List);
        });
        cx.simulate_keystrokes("shift-tab");
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert_eq!(view.ai_selection.focus_region, AiSelectionFocusRegion::Send);
        });
        cx.simulate_keystrokes("shift-tab");
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert_eq!(view.ai_selection.focus_region, AiSelectionFocusRegion::Input);
        });
    }

    #[gpui::test]
    fn tab_skips_the_saved_prompt_list_stop_while_it_is_empty(cx: &mut gpui::TestAppContext) {
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
        // A saved — not never-saved — empty list, unlike `Ok(None)`'s own
        // built-in default suggestion (see
        // `loading_saved_prompts_for_the_first_time_shows_the_built_in_default_suggestion`),
        // actually leaves the list empty, which is what this test exercises.
        view.update(cx, |view, cx| {
            let generation = view.ai_selection.prompts_load_generation;
            view.ai_selection.apply_saved_prompts(
                generation,
                Ok(Some(UserPrompts {
                    schema_version: hane_ai::USER_PROMPTS_SCHEMA_VERSION,
                    revision: 1,
                    next_id: 1,
                    prompts: Vec::new(),
                })),
            );
            cx.notify();
        });
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert!(view.ai_selection.saved_prompts.is_empty());
        });

        for expected in [
            AiSelectionFocusRegion::Send,
            AiSelectionFocusRegion::Close,
            AiSelectionFocusRegion::Input,
        ] {
            cx.simulate_keystrokes("tab");
            cx.run_until_parked();
            view.read_with(cx, |view, _| {
                assert_eq!(view.ai_selection.focus_region, expected);
                assert_eq!(view.ai_selection.list_focus_index, None);
            });
        }
    }

    #[gpui::test]
    fn up_down_move_the_saved_prompt_list_focus_only_while_the_list_itself_has_focus(
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
            view.ai_selection.saved_prompts =
                vec![saved_prompt(1, "直す"), saved_prompt(2, "要約する")];
            cx.notify();
        });
        cx.run_until_parked();

        // While the input (not the list) has focus, Up/Down must not move
        // anything in the list.
        cx.simulate_keystrokes("down");
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert_eq!(view.ai_selection.list_focus_index, None);
        });

        // Tab twice: Input -> Send -> List.
        cx.simulate_keystrokes("tab");
        cx.simulate_keystrokes("tab");
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert_eq!(view.ai_selection.focus_region, AiSelectionFocusRegion::List);
            assert_eq!(view.ai_selection.list_focus_index, Some(0));
        });

        cx.simulate_keystrokes("down");
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert_eq!(view.ai_selection.list_focus_index, Some(1));
        });

        // Wraps past the end.
        cx.simulate_keystrokes("down");
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert_eq!(view.ai_selection.list_focus_index, Some(0));
        });

        // And past the start going the other way.
        cx.simulate_keystrokes("up");
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert_eq!(view.ai_selection.list_focus_index, Some(1));
        });

        // Moving on to Close leaves the list's own index cleared, and
        // Up/Down there must not touch it again.
        cx.simulate_keystrokes("tab");
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert_eq!(view.ai_selection.focus_region, AiSelectionFocusRegion::Close);
        });
        cx.simulate_keystrokes("down");
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert_eq!(view.ai_selection.list_focus_index, None);
        });
    }

    #[gpui::test]
    fn dynamically_emptying_the_saved_prompt_list_moves_focus_off_of_it(
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
            view.ai_selection.saved_prompts = vec![saved_prompt(1, "直す")];
            cx.notify();
        });
        cx.run_until_parked();

        cx.simulate_keystrokes("tab");
        cx.simulate_keystrokes("tab");
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert_eq!(view.ai_selection.focus_region, AiSelectionFocusRegion::List);
            assert_eq!(view.ai_selection.list_focus_index, Some(0));
        });

        // A reload landing while the list holds focus (e.g. the prompts were
        // deleted from the settings page in another window, leaving a saved
        // — not never-saved — empty list) must not leave this pointed at an
        // item that no longer exists.
        view.update(cx, |view, cx| {
            let generation = view.ai_selection.prompts_load_generation;
            view.ai_selection.apply_saved_prompts(
                generation,
                Ok(Some(UserPrompts {
                    schema_version: hane_ai::USER_PROMPTS_SCHEMA_VERSION,
                    revision: 1,
                    next_id: 1,
                    prompts: Vec::new(),
                })),
            );
            cx.notify();
        });
        cx.run_until_parked();

        view.read_with(cx, |view, _| {
            assert!(view.ai_selection.saved_prompts.is_empty());
            assert_eq!(view.ai_selection.list_focus_index, None);
            assert_eq!(view.ai_selection.focus_region, AiSelectionFocusRegion::Send);
        });
    }

    #[gpui::test]
    fn loading_saved_prompts_for_the_first_time_shows_the_built_in_default_suggestion(
        cx: &mut gpui::TestAppContext,
    ) {
        // `Ok(None)` ("the file has never been saved", per `UserPromptsStore::
        // load`'s own doc comment) must show `initial_default_draft()`'s
        // suggestion, not an empty list — mirroring the settings page's own
        // blank-state default (`ai_prompts::AiPromptsState::apply_load`'s
        // `Ok(None)` branch). A genuine load failure (`Err`) must still show
        // the empty list, never this default.
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
            let generation = view.ai_selection.prompts_load_generation;
            view.ai_selection.apply_saved_prompts(generation, Ok(None));
            cx.notify();
        });
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert_eq!(view.ai_selection.saved_prompts, vec![default_saved_prompt()]);
        });

        view.update(cx, |view, cx| {
            let generation = view.ai_selection.prompts_load_generation;
            view.ai_selection.apply_saved_prompts(
                generation,
                Err(UserPromptsLoadError::Io(std::io::Error::other("boom"))),
            );
            cx.notify();
        });
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert!(view.ai_selection.saved_prompts.is_empty());
        });
    }

    #[gpui::test]
    fn tab_and_arrow_keys_never_leak_into_the_document_while_the_popup_is_open(
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
            view.ai_selection.saved_prompts = vec![saved_prompt(1, "直す")];
            cx.notify();
        });
        cx.run_until_parked();

        for keystroke in ["tab", "up", "down", "tab", "up", "down", "shift-tab", "tab"] {
            cx.simulate_keystrokes(keystroke);
            cx.run_until_parked();
        }

        view.read_with(cx, |view, _| {
            assert!(view.ai_selection.open);
            assert_eq!(view.editor().document().full_text(), "abcdef\n");
            assert!(!view.editor().can_undo());
        });
    }

    #[gpui::test]
    fn enter_at_send_focus_submits_the_free_text_instruction(cx: &mut gpui::TestAppContext) {
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

        // The free-text input holds real focus right after opening.
        cx.simulate_input("直す");
        cx.run_until_parked();

        cx.simulate_keystrokes("tab");
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert_eq!(view.ai_selection.focus_region, AiSelectionFocusRegion::Send);
        });

        cx.simulate_keystrokes("enter");
        cx.run_until_parked();

        view.read_with(cx, |view, _| {
            // No AI service is attached in this test, so the request is
            // rejected at admission rather than actually sent — but reaching
            // that rejection (and not, say, inserting a newline) confirms
            // Enter at `Send` submitted the free-text field's own value.
            assert_eq!(view.ai_selection.confirmed_instruction, "直す");
            assert_eq!(
                view.ai_selection.message.as_deref(),
                Some("AIサービスを利用できません。")
            );
            assert!(view.ai_selection.open);
            assert_eq!(view.editor().document().full_text(), "abcdef\n");
        });
    }

    #[gpui::test]
    fn enter_at_list_focus_sends_the_highlighted_saved_prompt_immediately(
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
            view.ai_selection.saved_prompts =
                vec![saved_prompt(1, "直す"), saved_prompt(2, "要約する")];
            cx.notify();
        });
        cx.run_until_parked();

        // Leaves text in the free-text field that must not be combined with
        // the saved prompt's own body below.
        cx.simulate_input("別の指示");
        cx.run_until_parked();

        cx.simulate_keystrokes("tab");
        cx.simulate_keystrokes("tab");
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert_eq!(view.ai_selection.focus_region, AiSelectionFocusRegion::List);
            assert_eq!(view.ai_selection.list_focus_index, Some(0));
        });

        cx.simulate_keystrokes("enter");
        cx.run_until_parked();

        view.read_with(cx, |view, _| {
            assert_eq!(view.ai_selection.confirmed_instruction, "直す");
            assert_eq!(
                view.ai_selection.message.as_deref(),
                Some("AIサービスを利用できません。")
            );
            assert!(view.ai_selection.open);
            assert_eq!(view.editor().document().full_text(), "abcdef\n");
        });
    }

    #[gpui::test]
    fn saved_prompt_with_surrounding_whitespace_is_sent_unchanged(
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

        // The saved prompt's own body has leading/trailing whitespace and an
        // internal newline, exactly as `ai_prompts.rs` would persist it. Only
        // the all-whitespace check may trim; the value actually sent must
        // stay byte-for-byte identical to what was saved.
        let padded_prompt = "  直して\n直す\t\n".to_owned();
        view.update(cx, |view, cx| {
            view.ai_selection.saved_prompts = vec![saved_prompt(1, &padded_prompt)];
            cx.notify();
        });
        cx.run_until_parked();

        cx.simulate_keystrokes("tab");
        cx.simulate_keystrokes("tab");
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert_eq!(view.ai_selection.focus_region, AiSelectionFocusRegion::List);
            assert_eq!(view.ai_selection.list_focus_index, Some(0));
        });

        cx.simulate_keystrokes("enter");
        cx.run_until_parked();

        view.read_with(cx, |view, _| {
            assert_eq!(view.ai_selection.confirmed_instruction, padded_prompt);
            assert_eq!(
                view.ai_selection.message.as_deref(),
                Some("AIサービスを利用できません。")
            );
            assert!(view.ai_selection.open);
            assert_eq!(view.editor().document().full_text(), "abcdef\n");
        });
    }

    #[gpui::test]
    fn whitespace_only_saved_prompt_is_rejected_without_sending(cx: &mut gpui::TestAppContext) {
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
            view.ai_selection.saved_prompts = vec![saved_prompt(1, " \n\t ")];
            let prompt = view.ai_selection.saved_prompts[0].prompt.clone();
            view.ai_selection_run_saved_prompt(prompt, cx);
            cx.notify();
        });
        cx.run_until_parked();

        view.read_with(cx, |view, _| {
            assert!(view.ai_selection.confirmed_instruction.is_empty());
            assert!(view.ai_selection.message.is_none());
            assert!(matches!(view.ai_selection.phase, AiSelectionPhase::Composing));
        });
    }

    #[gpui::test]
    fn enter_at_close_focus_closes_the_popup_and_returns_focus_to_the_document(
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

        // A saved — not never-saved — empty list, unlike `Ok(None)`'s own
        // built-in default suggestion, actually leaves the list empty, which
        // is what this test's "Input -> Send -> Close" tab order relies on.
        view.update(cx, |view, cx| {
            let generation = view.ai_selection.prompts_load_generation;
            view.ai_selection.apply_saved_prompts(
                generation,
                Ok(Some(UserPrompts {
                    schema_version: hane_ai::USER_PROMPTS_SCHEMA_VERSION,
                    revision: 1,
                    next_id: 1,
                    prompts: Vec::new(),
                })),
            );
            cx.notify();
        });
        cx.run_until_parked();

        // No saved prompts: Input -> Send -> Close.
        cx.simulate_keystrokes("tab");
        cx.simulate_keystrokes("tab");
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert_eq!(view.ai_selection.focus_region, AiSelectionFocusRegion::Close);
        });

        cx.simulate_keystrokes("enter");
        cx.run_until_parked();

        view.read_with(cx, |view, _| {
            assert!(!view.ai_selection.open);
            assert_eq!(view.editor().document().full_text(), "abcdef\n");
        });

        // Keyboard focus must have actually returned to the document body,
        // not just dropped the popup's own state.
        cx.simulate_input("X");
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert_eq!(view.editor().document().full_text(), "Xdef\n");
        });
    }

    #[gpui::test]
    fn enter_does_not_fall_back_to_a_leftover_focus_region_from_a_different_phase(
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

        cx.simulate_keystrokes("tab");
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert_eq!(view.ai_selection.focus_region, AiSelectionFocusRegion::Send);
        });

        // `Send` is a compose-only stop, not one of the submitting screen's
        // own (`Stop`). Forcing the phase to `Submitting` without going
        // through `start_request` (which would have landed `focus_region`
        // on `Stop` itself) leaves this stale `Send` behind; Enter must not
        // fall back to the compose screen's decision for it.
        view.update(cx, |view, cx| {
            view.ai_selection.phase = AiSelectionPhase::Submitting;
            cx.notify();
        });
        cx.run_until_parked();

        cx.simulate_keystrokes("enter");
        cx.run_until_parked();

        view.read_with(cx, |view, _| {
            assert!(view.ai_selection.open);
            assert!(matches!(view.ai_selection.phase, AiSelectionPhase::Submitting));
            assert!(view.ai_selection.confirmed_instruction.is_empty());
            assert_eq!(view.editor().document().full_text(), "abcdef\n");
        });
    }

    #[gpui::test]
    fn enter_while_composing_in_the_free_text_input_does_not_submit(
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

        let input = view
            .read_with(cx, |view, _| view.ai_selection.input.clone())
            .expect("input is initialized by open_ai_selection");
        cx.update(|window, app| {
            input.update(app, |state, cx| {
                gpui::EntityInputHandler::replace_and_mark_text_in_range(
                    state, None, "か", None, window, cx,
                );
            });
        });
        cx.run_until_parked();

        // This Enter only confirms the in-progress IME composition; it must
        // not also submit the free-text field as an instruction.
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();

        view.read_with(cx, |view, _| {
            assert!(view.ai_selection.open);
            assert!(matches!(view.ai_selection.phase, AiSelectionPhase::Composing));
            assert!(view.ai_selection.confirmed_instruction.is_empty());
            assert_eq!(view.editor().document().full_text(), "abcdef\n");
        });
    }

    #[gpui::test]
    fn escape_still_closes_the_popup_after_a_composing_escape_blurs_the_input(
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
        view.read_with(cx, |view, _| {
            assert!(view.ai_selection.input_focused);
        });

        let input = view
            .read_with(cx, |view, _| view.ai_selection.input.clone())
            .expect("input is initialized by open_ai_selection");
        cx.update(|window, app| {
            input.update(app, |state, cx| {
                gpui::EntityInputHandler::replace_and_mark_text_in_range(
                    state, None, "か", None, window, cx,
                );
            });
        });
        cx.run_until_parked();

        // The input still holds focus, and the composition just started is
        // still active, going into the first Escape below.
        view.read_with(cx, |view, _| {
            assert!(view.ai_selection.input_focused);
        });
        let marked_before_escape = cx.update(|window, app| {
            input.update(app, |state, cx| {
                gpui::EntityInputHandler::marked_text_range(state, window, cx)
            })
        });
        assert!(marked_before_escape.is_some());

        // While the input holds focus with an in-progress composition, Escape
        // must only cancel the composition, not close the popup.
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert!(view.ai_selection.open);
            assert!(view.ai_selection.input_focused);
        });

        // Tab away from the input (blurring it) without ever snapshotting a
        // fresh, non-composing Escape on it. The stale `escape_was_composing
        // == true` from above must not survive the blur.
        cx.simulate_keystrokes("tab");
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert_eq!(view.ai_selection.focus_region, AiSelectionFocusRegion::Send);
            assert!(!view.ai_selection.input_focused);
        });

        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert!(!view.ai_selection.open);
        });
    }

    #[gpui::test]
    fn tab_after_pointer_refocusing_the_input_reaches_send(cx: &mut gpui::TestAppContext) {
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

        // Move logical focus away from the input, then click the input with
        // a real pointer event (bypassing the Tab handler).
        cx.simulate_keystrokes("tab");
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert_eq!(view.ai_selection.focus_region, AiSelectionFocusRegion::Send);
        });
        let point = cx
            .debug_bounds("ai-selection-input")
            .expect("ai instruction input painted")
            .center();
        cx.simulate_click(point, gpui::Modifiers::none());
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert_eq!(view.ai_selection.focus_region, AiSelectionFocusRegion::Input);
            assert!(view.ai_selection.input_focused);
        });

        cx.simulate_keystrokes("tab");
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert_eq!(view.ai_selection.focus_region, AiSelectionFocusRegion::Send);
        });
    }

    // RF3-A P2 contract test (Issue #304, design spec §9 R09): this pins the
    // `CancelComposition` short-circuit order's `ai_selection_blocks_editor_keys`
    // branch in `actions.rs` — reaching it must `return` before
    // `perform_cancel_composition`/`after_input` ever run for the body, since
    // `after_input` unconditionally calls `AiSelectionState::invalidate_for_edit`
    // and would otherwise close this still-open popup and surface the
    // stale-result status message on the very Escape that was only supposed
    // to cancel the free-text input's own composition.
    //
    // Synthetic overlap: the real UI never lets the body hold its own IME
    // composition while this popup is open (`open_ai_selection` refuses to
    // open over one, and `ai_selection_blocks_editor_keys` keeps the body out
    // of the input-capture path once it is open), so this seeds the body's
    // composition directly on its `Editor`, bypassing normal focus routing,
    // purely to make a leak from the input's Escape into the body's own
    // composition-cancel observable in a single test.
    #[gpui::test]
    fn escape_that_cancels_the_ai_inputs_composition_does_not_leak_into_the_bodys_composition_cancel_or_after_input(
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
        view.read_with(cx, |view, _| {
            assert!(view.ai_selection.open);
            assert!(view.ai_selection.input_focused);
        });

        let input = view
            .read_with(cx, |view, _| view.ai_selection.input.clone())
            .expect("input is initialized by open_ai_selection");
        cx.update(|window, app| {
            input.update(app, |state, cx| {
                gpui::EntityInputHandler::replace_and_mark_text_in_range(
                    state, None, "か", None, window, cx,
                );
            });
        });
        cx.run_until_parked();

        // Synthetic: see the comment above this test for why the body can
        // never really be composing here.
        view.update(cx, |view, cx| {
            view.editor_mut()
                .replace_and_mark_text(None, "ば", None)
                .unwrap();
            cx.notify();
        });
        cx.run_until_parked();
        let body_source_before_escape = view.read_with(cx, |view, _| {
            assert!(view.editor().ime().is_some());
            view.editor().document().full_text()
        });

        // The input still holds focus with its own in-progress composition,
        // going into this Escape.
        let marked_before_escape = cx.update(|window, app| {
            input.update(app, |state, cx| {
                gpui::EntityInputHandler::marked_text_range(state, window, cx)
            })
        });
        assert!(marked_before_escape.is_some());

        cx.simulate_keystrokes("escape");
        cx.run_until_parked();

        view.read_with(cx, |view, _| {
            assert!(
                view.ai_selection.open,
                "a leak into the body's after_input would invalidate and close the still-open popup"
            );
            assert!(
                view.status.is_none(),
                "a leak into after_input's invalidate_for_edit would surface the stale-result status message"
            );
            assert!(
                view.editor().ime().is_some(),
                "the body's own composition, seeded only to detect a leak, must survive untouched"
            );
            assert_eq!(
                view.editor().document().full_text(),
                body_source_before_escape,
                "the body's own composition source must be untouched by the input's Escape"
            );
        });

        let marked_after_escape = cx.update(|window, app| {
            input.update(app, |state, cx| {
                gpui::EntityInputHandler::marked_text_range(state, window, cx)
            })
        });
        assert!(
            marked_after_escape.is_none(),
            "the AI input's own composition must still be cancelled by this Escape"
        );
    }

    fn open_popup_for_keyboard_tests(
        cx: &mut gpui::TestAppContext,
    ) -> (Entity<EditorView>, &mut gpui::VisualTestContext) {
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
        (view, cx)
    }

    // PR #450 review: the reviewing screen previously had no focus stops of
    // its own, so Tab/Shift+Tab only cycled the compose screen's stops and
    // Enter never decided anything there — leaving the result screen
    // keyboard-unreachable. These stops follow the same
    // primary-action/secondary-actions/dismiss order as the compose screen's
    // own (`ai_selection_composing_focus_stops`): 置き換える, 再生成, 破棄,
    // then × last.
    #[gpui::test]
    fn tab_cycles_focus_through_the_reviewing_screens_own_stops(cx: &mut gpui::TestAppContext) {
        let (view, cx) = open_popup_for_keyboard_tests(cx);
        view.update(cx, |view, cx| {
            view.ai_selection.phase = AiSelectionPhase::Reviewing {
                result: "XYZ".to_owned(),
                unchanged: false,
            };
            view.ai_selection.focus_region = AiSelectionFocusRegion::Replace;
            cx.notify();
        });
        cx.run_until_parked();

        for expected in [
            AiSelectionFocusRegion::Regenerate,
            AiSelectionFocusRegion::Discard,
            AiSelectionFocusRegion::Close,
            AiSelectionFocusRegion::Replace,
        ] {
            cx.simulate_keystrokes("tab");
            cx.run_until_parked();
            view.read_with(cx, |view, _| {
                assert_eq!(view.ai_selection.focus_region, expected);
            });
        }

        // Shift+Tab walks the exact same stops backward.
        cx.simulate_keystrokes("shift-tab");
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert_eq!(view.ai_selection.focus_region, AiSelectionFocusRegion::Close);
        });

        view.read_with(cx, |view, _| {
            assert_eq!(view.editor().document().full_text(), "abcdef\n");
            assert!(!view.editor().can_undo());
        });
    }

    #[gpui::test]
    fn tab_skips_the_replace_stop_in_the_reviewing_screen_once_the_result_is_unchanged(
        cx: &mut gpui::TestAppContext,
    ) {
        let (view, cx) = open_popup_for_keyboard_tests(cx);
        view.update(cx, |view, cx| {
            view.ai_selection.phase = AiSelectionPhase::Reviewing {
                result: "abc".to_owned(),
                unchanged: true,
            };
            view.ai_selection.focus_region = AiSelectionFocusRegion::Regenerate;
            cx.notify();
        });
        cx.run_until_parked();

        for expected in [
            AiSelectionFocusRegion::Discard,
            AiSelectionFocusRegion::Close,
            // Wraps straight back to `Regenerate`, never landing on
            // `Replace` — there is nothing for it to apply.
            AiSelectionFocusRegion::Regenerate,
        ] {
            cx.simulate_keystrokes("tab");
            cx.run_until_parked();
            view.read_with(cx, |view, _| {
                assert_eq!(view.ai_selection.focus_region, expected);
            });
        }
    }

    #[gpui::test]
    fn enter_at_the_reviewing_replace_stop_applies_the_result(cx: &mut gpui::TestAppContext) {
        let (view, cx) = open_popup_for_keyboard_tests(cx);
        view.update(cx, |view, cx| {
            view.ai_selection.phase = AiSelectionPhase::Reviewing {
                result: "XYZ".to_owned(),
                unchanged: false,
            };
            view.ai_selection.focus_region = AiSelectionFocusRegion::Replace;
            cx.notify();
        });
        cx.run_until_parked();

        cx.simulate_keystrokes("enter");
        cx.run_until_parked();

        view.read_with(cx, |view, _| {
            assert!(!view.ai_selection.open);
            assert_eq!(view.editor().document().full_text(), "XYZdef\n");
            assert!(view.editor().can_undo());
        });
    }

    #[gpui::test]
    fn enter_at_the_reviewing_regenerate_stop_starts_a_fresh_request(
        cx: &mut gpui::TestAppContext,
    ) {
        let (view, cx) = open_popup_for_keyboard_tests(cx);
        view.update(cx, |view, cx| {
            view.ai_selection.confirmed_instruction = "直す".to_owned();
            view.ai_selection.phase = AiSelectionPhase::Reviewing {
                result: "XYZ".to_owned(),
                unchanged: false,
            };
            view.ai_selection.focus_region = AiSelectionFocusRegion::Regenerate;
            cx.notify();
        });
        cx.run_until_parked();

        cx.simulate_keystrokes("enter");
        cx.run_until_parked();

        view.read_with(cx, |view, _| {
            // No AI service is attached in this test, so the request is
            // rejected at admission — but the rejection message (rather
            // than, say, a replace/discard) confirms Enter at `Regenerate`
            // reached `ai_selection_regenerate` and not some other stop's
            // decision.
            assert!(view.ai_selection.open);
            assert_eq!(
                view.ai_selection.message.as_deref(),
                Some("AIサービスを利用できません。")
            );
            assert_eq!(view.editor().document().full_text(), "abcdef\n");
        });
    }

    #[gpui::test]
    fn enter_at_the_reviewing_discard_or_close_stop_closes_without_touching_the_document(
        cx: &mut gpui::TestAppContext,
    ) {
        for focus in [AiSelectionFocusRegion::Discard, AiSelectionFocusRegion::Close] {
            let (view, cx) = open_popup_for_keyboard_tests(cx);
            view.update(cx, |view, cx| {
                view.ai_selection.phase = AiSelectionPhase::Reviewing {
                    result: "XYZ".to_owned(),
                    unchanged: false,
                };
                view.ai_selection.focus_region = focus;
                cx.notify();
            });
            cx.run_until_parked();

            cx.simulate_keystrokes("enter");
            cx.run_until_parked();

            view.read_with(cx, |view, _| {
                assert!(!view.ai_selection.open);
                assert_eq!(view.editor().document().full_text(), "abcdef\n");
                assert!(!view.editor().can_undo());
            });

            // Keyboard focus must have actually returned to the document
            // body, not just dropped the popup's own state — typing now
            // replaces the still-selected "abc" rather than being swallowed.
            cx.simulate_input("X");
            cx.run_until_parked();
            view.read_with(cx, |view, _| {
                assert_eq!(view.editor().document().full_text(), "Xdef\n");
            });
        }
    }

    // PR #450 review: the submitting screen also had no focus stop of its
    // own, so Tab/Enter there only swallowed the key without ever reaching
    // the existing "停止" control.
    #[gpui::test]
    fn tab_reaches_and_enter_activates_the_submitting_screens_stop_control(
        cx: &mut gpui::TestAppContext,
    ) {
        let (view, cx) = open_popup_for_keyboard_tests(cx);
        view.update(cx, |view, cx| {
            view.ai_selection.pending = Some(pending(1, 1));
            view.ai_selection.phase = AiSelectionPhase::Submitting;
            view.ai_selection.focus_region = AiSelectionFocusRegion::Stop;
            cx.notify();
        });
        cx.run_until_parked();

        // A single-stop screen: Tab/Shift+Tab never leaks into the document
        // and leaves the one stop still focused.
        cx.simulate_keystrokes("tab");
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert_eq!(view.ai_selection.focus_region, AiSelectionFocusRegion::Stop);
        });

        cx.simulate_keystrokes("enter");
        cx.run_until_parked();

        view.read_with(cx, |view, _| {
            assert!(view.ai_selection.open);
            assert!(matches!(view.ai_selection.phase, AiSelectionPhase::Composing));
            assert!(view.ai_selection.pending.is_none());
            assert_eq!(view.editor().document().full_text(), "abcdef\n");
            assert!(!view.editor().can_undo());
        });
    }

    // PR #450 review: `ai_selection_start()` admitted a request through
    // `start_request()` without re-checking `text_transform_connection_ready`
    // against a fresh snapshot, so free-text Enter, Send/List Enter, a saved
    // prompt's own click, and Regenerate could all enqueue a real request
    // while the connection was not actually ready — the compose screen's
    // disabled Send button was only a rendering-level gate, not an admission
    // one. Every one of those paths funnels through the same `start_request`
    // chokepoint, so this exercises each of them against one real, freshly
    // spawned (and therefore still `OwnershipState::Unknown`, i.e. not
    // ready) `AiServiceHandle` and asserts none of them ever admits a
    // request (`handle.snapshot().busy` stays `None` throughout) or leaves
    // `pending`/`phase` as if one had been.
    #[gpui::test]
    fn connection_not_ready_blocks_every_start_path_from_enqueueing(
        cx: &mut gpui::TestAppContext,
    ) {
        use hane_ai::{
            AiService, AiServiceConfig, FakeCredentialStore, ShellEnvironmentPolicyFormat,
            SystemBrowserOpener,
        };
        use std::sync::Arc;
        use std::time::{SystemTime, UNIX_EPOCH};

        let app_data_root = std::env::temp_dir().join(format!(
            "hane-ui-ai-selection-ready-gate-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&app_data_root).unwrap();
        let service = AiService::spawn(AiServiceConfig {
            app_data_root: app_data_root.clone(),
            binary_path: app_data_root.join("missing-codex-app-server"),
            binary_resolution_error: None,
            credential_store: Arc::new(FakeCredentialStore::new()),
            browser_opener: Arc::new(SystemBrowserOpener),
            shell_env_format: ShellEnvironmentPolicyFormat::Filters,
        })
        .unwrap();
        let handle = service.handle();

        let (view, cx) = open_popup_for_keyboard_tests(cx);
        view.update(cx, |view, cx| {
            view.ai_selection.service = Some(handle.clone());
            cx.notify();
        });
        cx.run_until_parked();
        // Left un-probed/unconfigured on purpose: `OwnershipState::Unknown`
        // is not `Owned`, so `text_transform_connection_ready` reports not
        // ready for the whole test, the same as a connection the user has
        // not finished setting up.
        assert_ne!(handle.snapshot().ownership, OwnershipState::Owned);

        // 1) Free-text Enter: the pinned input's own `InputEvent::PressEnter`.
        cx.simulate_input("直して");
        cx.run_until_parked();
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert!(matches!(view.ai_selection.phase, AiSelectionPhase::Composing));
            assert!(view.ai_selection.pending.is_none());
        });
        assert!(handle.snapshot().busy.is_none());

        // 2) Logical focus on `Send`, then Enter: the Send button's own
        // click takes the same `ai_selection_submit_free_text` path.
        cx.simulate_keystrokes("tab");
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert_eq!(view.ai_selection.focus_region, AiSelectionFocusRegion::Send);
        });
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert!(matches!(view.ai_selection.phase, AiSelectionPhase::Composing));
            assert!(view.ai_selection.pending.is_none());
        });
        assert!(handle.snapshot().busy.is_none());

        // 3) A saved prompt's own row: Enter at `List` takes the same
        // `ai_selection_run_saved_prompt` path as clicking it.
        view.update(cx, |view, cx| {
            view.ai_selection.saved_prompts = vec![saved_prompt(1, "直す")];
            cx.notify();
        });
        cx.run_until_parked();
        cx.simulate_keystrokes("tab");
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert_eq!(view.ai_selection.focus_region, AiSelectionFocusRegion::List);
        });
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert_eq!(view.ai_selection.confirmed_instruction, "直す");
            assert!(matches!(view.ai_selection.phase, AiSelectionPhase::Composing));
            assert!(view.ai_selection.pending.is_none());
        });
        assert!(handle.snapshot().busy.is_none());

        // 4) Regenerate, from the reviewing screen: it never goes through
        // `ai_selection_start`, so it must re-check readiness on its own
        // way into the same `start_request` chokepoint.
        view.update(cx, |view, cx| {
            view.ai_selection.phase = AiSelectionPhase::Reviewing {
                result: "XYZ".to_owned(),
                unchanged: false,
            };
            view.ai_selection.focus_region = AiSelectionFocusRegion::Regenerate;
            cx.notify();
        });
        cx.run_until_parked();
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert!(view.ai_selection.open);
            assert!(matches!(view.ai_selection.phase, AiSelectionPhase::Composing));
            assert!(view.ai_selection.pending.is_none());
            assert_eq!(view.editor().document().full_text(), "abcdef\n");
        });
        assert!(handle.snapshot().busy.is_none());

        drop(handle);
        drop(service);
        std::fs::remove_dir_all(app_data_root).unwrap();
    }
}
