use super::EditorView;
use crate::theme::Theme;
use gpui::{
    App, AppContext, Context, Entity, Focusable, InteractiveElement, IntoElement, ParentElement,
    StatefulInteractiveElement, Styled, Window, div, px, rgb,
};
use gpui_component::Disableable;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{Input, InputContentType, InputState};
use gpui_component::menu::{DropdownMenu, PopupMenuItem};
use gpui_component::radio::Radio;
use gpui_component::{Icon, Sizable};
use hane_ai::{
    AccountState, ActiveConnection, AdmissionError, AiCommand, AiServiceHandle, AiSettings,
    AiSnapshot, ChatGptConnectionSettings, CustomConnectionSettings, LoginState, ModelListState,
    OperationId, OwnershipState, PersistenceState, ProbeErrorCode, ProbeStatus,
    SafeOperationResult, ServiceBusyReason,
};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

// T00 passed for the pinned standalone 0.157.1 profile; see the evidence log.
const T00_PROBE_GATE_PASSED: bool = true;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum LeaveTarget {
    General,
    Close,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CredentialEdit {
    Keep,
    Replace,
    Delete,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CustomProviderPreset {
    OpenAi,
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PendingConfirmation {
    Logout,
    DeleteKey(Option<LeaveTarget>),
}

const OPENAI_PROVIDER_NAME: &str = "OpenAI";
const OPENAI_PROVIDER_URL: &str = "https://api.openai.com/v1";

fn connection_name(connection: ActiveConnection) -> &'static str {
    match connection {
        ActiveConnection::ChatGpt => "ChatGPT / Codex",
        ActiveConnection::Custom => "APIキー接続",
    }
}

fn custom_key_action_label(registered: bool) -> &'static str {
    if registered { "変更" } else { "登録" }
}

fn custom_key_guidance(edit: CredentialEdit, registered: bool) -> &'static str {
    match (edit, registered) {
        (CredentialEdit::Keep, true) => {
            "登録済みのAPIキーは「変更」または「削除」するまで保持されます。値は画面に表示しません。応答確認は別の操作です。"
        }
        (CredentialEdit::Keep, false) => {
            "「登録」を押すと入力欄が開きます。入力後に「保存して適用」を押してください。APIキーは保存後も表示しません。接続確認は別操作です。"
        }
        (CredentialEdit::Replace, _) => {
            "新しいAPIキーを入力して「保存して適用」を押してください。キーは保存後も表示しません。保存・適用だけでは接続確認を行いません。"
        }
        (CredentialEdit::Delete, _) => {
            "削除は「保存して適用」を押した後に確定します。保存前なら「元に戻す」で取り消せます。"
        }
    }
}

fn custom_settings_match_openai(name: &str, base_url: &str) -> bool {
    name.trim() == OPENAI_PROVIDER_NAME
        && base_url.trim().trim_end_matches('/') == OPENAI_PROVIDER_URL
}

fn endpoint_changed_with_registered_key(
    saved: Option<&CustomConnectionSettings>,
    draft_base_url: &str,
    credential_edit: CredentialEdit,
) -> bool {
    saved.is_some_and(|saved| {
        saved.credential_ref.is_some()
            && credential_edit == CredentialEdit::Keep
            && saved.base_url.trim() != draft_base_url.trim()
    })
}

struct AiInputs {
    chatgpt_model: Entity<InputState>,
    custom_name: Entity<InputState>,
    custom_base_url: Entity<InputState>,
    custom_model: Entity<InputState>,
    custom_api_key: Entity<InputState>,
}

pub(super) struct AiSettingsPage {
    service: Option<AiServiceHandle>,
    snapshot: AiSnapshot,
    inputs: Option<AiInputs>,
    active_connection: ActiveConnection,
    custom_preset: CustomProviderPreset,
    custom_details_open: bool,
    credential_edit: CredentialEdit,
    message: Option<String>,
    leave_prompt: Option<LeaveTarget>,
    pending_connection_switch: Option<ActiveConnection>,
    pending_confirmation: Option<PendingConfirmation>,
    diagnostics_open: bool,
    save_then_leave: Option<(OperationId, LeaveTarget)>,
    route_ready: Option<LeaveTarget>,
    subscription_started: bool,
}

impl Default for AiSettingsPage {
    fn default() -> Self {
        Self {
            service: None,
            snapshot: AiSnapshot::default(),
            inputs: None,
            active_connection: ActiveConnection::ChatGpt,
            custom_preset: CustomProviderPreset::Other,
            custom_details_open: false,
            credential_edit: CredentialEdit::Keep,
            message: None,
            leave_prompt: None,
            pending_connection_switch: None,
            pending_confirmation: None,
            diagnostics_open: false,
            save_then_leave: None,
            route_ready: None,
            subscription_started: false,
        }
    }
}

impl AiSettingsPage {
    pub(super) fn attach(
        &mut self,
        service: Option<AiServiceHandle>,
        cx: &mut Context<EditorView>,
    ) {
        let Some(service) = service else {
            self.message = Some(
                "AIサービスを開始できませんでした。Markdown編集は引き続き利用できます。".to_owned(),
            );
            return;
        };
        self.snapshot = service.snapshot();
        let updates = Arc::new(Mutex::new(service.subscribe()));
        let snapshot_service = service.clone();
        self.service = Some(service);
        if self.subscription_started {
            return;
        }
        self.subscription_started = true;
        let view = cx.entity().downgrade();
        cx.spawn(async move |_, cx| {
            loop {
                let updates = updates.clone();
                let result = cx
                    .background_executor()
                    .spawn(async move {
                        updates
                            .lock()
                            .expect("AI snapshot receiver lock")
                            .recv_timeout(Duration::from_millis(200))
                    })
                    .await;
                match result {
                    Ok(()) => {
                        let snapshot_service = snapshot_service.clone();
                        if view
                            .update(cx, move |view, cx| {
                                let snapshot = snapshot_service.snapshot();
                                view.ai_settings.apply_snapshot(snapshot);
                                cx.notify();
                            })
                            .is_err()
                        {
                            break;
                        }
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        if view.update(cx, |_, _| ()).is_err() {
                            break;
                        }
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                }
            }
        })
        .detach();
    }

    pub(super) fn begin_settings_session(&mut self) {
        self.inputs = None;
        self.active_connection = self.snapshot.settings.active_connection;
        self.custom_preset =
            self.snapshot
                .settings
                .custom
                .as_ref()
                .map_or(CustomProviderPreset::Other, |custom| {
                    if custom_settings_match_openai(&custom.name, &custom.base_url) {
                        CustomProviderPreset::OpenAi
                    } else {
                        CustomProviderPreset::Other
                    }
                });
        self.credential_edit = CredentialEdit::Keep;
        self.custom_details_open = false;
        self.message = None;
        self.leave_prompt = None;
        self.pending_connection_switch = None;
        self.pending_confirmation = None;
        self.diagnostics_open = false;
        self.save_then_leave = None;
        self.route_ready = None;
    }

    pub(super) fn close_settings(&mut self) {
        // Dropping the masked InputState releases the transient secret field.
        // It does not cancel app-owned OAuth or Probe operations.
        self.inputs = None;
        self.credential_edit = CredentialEdit::Keep;
        self.custom_details_open = false;
        self.leave_prompt = None;
        self.pending_connection_switch = None;
        self.pending_confirmation = None;
        self.save_then_leave = None;
        self.route_ready = None;
    }

    pub(super) fn activate(&mut self, _cx: &mut Context<EditorView>) {
        self.message = None;
        let Some(service) = &self.service else {
            return;
        };
        match service.try_submit(AiCommand::OpenSettings) {
            Ok(_) => {}
            Err(AdmissionError::Busy) => {
                // Existing app-owned work continues; the subscription will
                // supply its eventual snapshot without restarting it.
            }
            Err(error) => self.message = Some(admission_message(error)),
        }
    }

    fn apply_snapshot(&mut self, snapshot: AiSnapshot) {
        self.snapshot = snapshot;
        if let Some((operation, target)) = self.save_then_leave
            && self.snapshot.busy.is_none()
            && self
                .snapshot
                .last_result
                .as_ref()
                .is_some_and(|(id, result)| {
                    *id == operation && *result == SafeOperationResult::Succeeded
                })
            && matches!(
                self.snapshot.persistence,
                PersistenceState::Saved | PersistenceState::Clean
            )
        {
            self.save_then_leave = None;
            self.route_ready = Some(target);
        }
    }

    pub(super) fn is_dirty(&self, cx: &App) -> bool {
        let Some(inputs) = &self.inputs else {
            return false;
        };
        let saved = &self.snapshot.settings;
        let (custom_name, custom_base_url, custom_model) = self.custom_draft_values(inputs, cx);
        self.active_connection != saved.active_connection
            || value(&inputs.chatgpt_model, cx)
                != saved.chatgpt.model_id.as_deref().unwrap_or_default()
            || custom_name
                != saved
                    .custom
                    .as_ref()
                    .map_or("", |custom| custom.name.as_str())
            || custom_base_url
                != saved
                    .custom
                    .as_ref()
                    .map_or("", |custom| custom.base_url.as_str())
            || custom_model
                != saved
                    .custom
                    .as_ref()
                    .map_or("", |custom| custom.model_id.as_str())
            || self.credential_edit != CredentialEdit::Keep
    }

    fn connection_draft_is_dirty(&self, connection: ActiveConnection, cx: &App) -> bool {
        let Some(inputs) = &self.inputs else {
            return false;
        };
        match connection {
            ActiveConnection::ChatGpt => {
                value(&inputs.chatgpt_model, cx)
                    != self
                        .snapshot
                        .settings
                        .chatgpt
                        .model_id
                        .as_deref()
                        .unwrap_or_default()
            }
            ActiveConnection::Custom => {
                let saved = self.snapshot.settings.custom.as_ref();
                let (name, base_url, model) = self.custom_draft_values(inputs, cx);
                name != saved.map_or("", |custom| custom.name.as_str())
                    || base_url != saved.map_or("", |custom| custom.base_url.as_str())
                    || model != saved.map_or("", |custom| custom.model_id.as_str())
                    || self.credential_edit != CredentialEdit::Keep
            }
        }
    }

    fn custom_draft_values(&self, inputs: &AiInputs, cx: &App) -> (String, String, String) {
        let (name, base_url) = match self.custom_preset {
            CustomProviderPreset::OpenAi => (
                OPENAI_PROVIDER_NAME.to_owned(),
                OPENAI_PROVIDER_URL.to_owned(),
            ),
            CustomProviderPreset::Other => (
                value(&inputs.custom_name, cx),
                value(&inputs.custom_base_url, cx),
            ),
        };
        (name, base_url, value(&inputs.custom_model, cx))
    }

    fn request_connection_switch(
        &mut self,
        next: ActiveConnection,
        window: &mut Window,
        cx: &mut Context<EditorView>,
    ) {
        if next == self.active_connection {
            return;
        }
        if self.connection_draft_is_dirty(self.active_connection, cx) {
            self.pending_connection_switch = Some(next);
        } else {
            self.active_connection = next;
            self.message = None;
        }
        let _ = window;
    }

    fn discard_connection_draft(
        &mut self,
        connection: ActiveConnection,
        window: &mut Window,
        cx: &mut Context<EditorView>,
    ) {
        let Some(inputs) = &self.inputs else {
            return;
        };
        match connection {
            ActiveConnection::ChatGpt => set_value(
                &inputs.chatgpt_model,
                self.snapshot
                    .settings
                    .chatgpt
                    .model_id
                    .as_deref()
                    .unwrap_or_default(),
                window,
                cx,
            ),
            ActiveConnection::Custom => {
                let saved = self.snapshot.settings.custom.as_ref();
                set_value(
                    &inputs.custom_name,
                    saved.map_or("", |custom| custom.name.as_str()),
                    window,
                    cx,
                );
                set_value(
                    &inputs.custom_base_url,
                    saved.map_or("", |custom| custom.base_url.as_str()),
                    window,
                    cx,
                );
                set_value(
                    &inputs.custom_model,
                    saved.map_or("", |custom| custom.model_id.as_str()),
                    window,
                    cx,
                );
                set_value(&inputs.custom_api_key, "", window, cx);
                self.custom_preset = saved.map_or(CustomProviderPreset::Other, |custom| {
                    if custom_settings_match_openai(&custom.name, &custom.base_url) {
                        CustomProviderPreset::OpenAi
                    } else {
                        CustomProviderPreset::Other
                    }
                });
                self.credential_edit = CredentialEdit::Keep;
            }
        }
        self.pending_connection_switch = None;
        self.message = None;
    }

    pub(super) fn confirm_leave(&mut self, target: LeaveTarget) {
        self.leave_prompt = Some(target);
    }

    pub(super) fn take_ready_route(&mut self) -> Option<LeaveTarget> {
        self.route_ready.take()
    }

    pub(super) fn input_has_focus(&self, window: &Window, cx: &App) -> bool {
        self.inputs.as_ref().is_some_and(|inputs| {
            [
                &inputs.chatgpt_model,
                &inputs.custom_name,
                &inputs.custom_base_url,
                &inputs.custom_model,
                &inputs.custom_api_key,
            ]
            .into_iter()
            .any(|input| input.read(cx).focus_handle(cx).is_focused(window))
        })
    }

    fn needs_durability_reconfirmation(&self) -> bool {
        self.snapshot.recovery_required
            && self.snapshot.persistence == PersistenceState::DurabilityUnconfirmed
    }

    fn settings_editable(&self) -> bool {
        let durability_reconfirmation = self.needs_durability_reconfirmation();
        self.snapshot.ownership == OwnershipState::Owned
            && self.snapshot.busy.is_none()
            && (durability_reconfirmation
                || (!self.snapshot.recovery_required
                    && matches!(
                        self.snapshot.persistence,
                        PersistenceState::Clean | PersistenceState::Saved
                    )))
    }

    fn ensure_inputs(&mut self, window: &mut Window, cx: &mut Context<EditorView>) {
        if self.inputs.is_some()
            || self.snapshot.ownership != OwnershipState::Owned
            || (self.snapshot.recovery_required && !self.needs_durability_reconfirmation())
            || self.snapshot.busy.is_some()
        {
            return;
        }
        let settings = &self.snapshot.settings;
        let custom = settings.custom.as_ref();
        let chatgpt_model = settings.chatgpt.model_id.as_deref().unwrap_or_default();
        let custom_name = custom.map_or("", |custom| custom.name.as_str());
        let custom_base_url = custom.map_or("", |custom| custom.base_url.as_str());
        let custom_model = custom.map_or("", |custom| custom.model_id.as_str());
        self.active_connection = settings.active_connection;
        self.custom_preset = if custom
            .is_some_and(|custom| custom_settings_match_openai(&custom.name, &custom.base_url))
        {
            CustomProviderPreset::OpenAi
        } else {
            CustomProviderPreset::Other
        };
        self.inputs = Some(AiInputs {
            chatgpt_model: cx.new(|cx| {
                InputState::new(window, cx)
                    .default_value(chatgpt_model)
                    .placeholder("一覧から選択、またはモデルIDを入力")
            }),
            custom_name: cx.new(|cx| {
                InputState::new(window, cx)
                    .default_value(custom_name)
                    .placeholder("Provider名")
            }),
            custom_base_url: cx.new(|cx| {
                InputState::new(window, cx)
                    .default_value(custom_base_url)
                    .placeholder("https://provider.example/v1")
            }),
            custom_model: cx.new(|cx| {
                InputState::new(window, cx)
                    .default_value(custom_model)
                    .placeholder("model-id")
            }),
            custom_api_key: cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder("新しいAPIキー")
                    .masked(true)
            }),
        });
    }

    fn reset_draft(&mut self, window: &mut Window, cx: &mut Context<EditorView>) {
        let Some(inputs) = &self.inputs else {
            return;
        };
        let settings = &self.snapshot.settings;
        let custom = settings.custom.as_ref();
        set_value(
            &inputs.chatgpt_model,
            settings.chatgpt.model_id.as_deref().unwrap_or_default(),
            window,
            cx,
        );
        set_value(
            &inputs.custom_name,
            custom.map_or("", |custom| custom.name.as_str()),
            window,
            cx,
        );
        set_value(
            &inputs.custom_base_url,
            custom.map_or("", |custom| custom.base_url.as_str()),
            window,
            cx,
        );
        set_value(
            &inputs.custom_model,
            custom.map_or("", |custom| custom.model_id.as_str()),
            window,
            cx,
        );
        set_value(&inputs.custom_api_key, "", window, cx);
        self.active_connection = settings.active_connection;
        self.custom_preset = custom.map_or(CustomProviderPreset::Other, |custom| {
            if custom_settings_match_openai(&custom.name, &custom.base_url) {
                CustomProviderPreset::OpenAi
            } else {
                CustomProviderPreset::Other
            }
        });
        self.credential_edit = CredentialEdit::Keep;
        self.custom_details_open = false;
        self.leave_prompt = None;
        self.pending_connection_switch = None;
        self.pending_confirmation = None;
        self.message = None;
    }

    fn draft_settings(&self, cx: &App) -> Option<AiSettings> {
        let inputs = self.inputs.as_ref()?;
        let mut proposed = self.snapshot.settings.clone();
        proposed.active_connection = self.active_connection;
        let model = value(&inputs.chatgpt_model, cx);
        proposed.chatgpt = ChatGptConnectionSettings {
            model_id: (!model.is_empty()).then_some(model),
        };
        let existing = proposed.custom.as_ref();
        let (name, base_url, model_id) = self.custom_draft_values(inputs, cx);
        if !name.is_empty() || !base_url.is_empty() || !model_id.is_empty() || existing.is_some() {
            proposed.custom = Some(CustomConnectionSettings {
                id: existing.map_or_else(
                    || "hane-custom-provider".to_owned(),
                    |custom| custom.id.clone(),
                ),
                name,
                base_url,
                model_id,
                credential_ref: existing.and_then(|custom| custom.credential_ref.clone()),
            });
        }
        Some(proposed)
    }

    fn save(
        &mut self,
        window: &mut Window,
        cx: &mut Context<EditorView>,
        target: Option<LeaveTarget>,
    ) {
        self.save_inner(window, cx, target, false);
    }

    fn save_confirmed(
        &mut self,
        window: &mut Window,
        cx: &mut Context<EditorView>,
        target: Option<LeaveTarget>,
    ) {
        self.save_inner(window, cx, target, true);
    }

    fn save_inner(
        &mut self,
        window: &mut Window,
        cx: &mut Context<EditorView>,
        target: Option<LeaveTarget>,
        delete_confirmed: bool,
    ) {
        self.message = None;
        let Some(service) = &self.service else {
            self.message = Some("AIサービスを利用できません。".to_owned());
            return;
        };
        let Some(mut proposed) = self.draft_settings(cx) else {
            self.message =
                Some("AI設定を読み込めていません。もう一度開き直してください。".to_owned());
            return;
        };
        let existing_custom = self.snapshot.settings.custom.as_ref();
        let proposed_base_url = proposed
            .custom
            .as_ref()
            .map_or("", |custom| custom.base_url.as_str());
        if endpoint_changed_with_registered_key(
            existing_custom,
            proposed_base_url,
            self.credential_edit,
        ) {
            self.message = Some(
                "接続先が変わっています。安全のため、登録済みkeyは新しい接続先へ引き継ぎません。新しいkeyを登録するか、keyを削除してから保存してください。"
                    .to_owned(),
            );
            return;
        }
        if self.active_connection == ActiveConnection::Custom
            && let Err(error) = hane_ai::validate_base_url(proposed_base_url)
        {
            self.message = Some(base_url_validation_message(error).to_owned());
            return;
        }
        if self.active_connection == ActiveConnection::Custom
            && !proposed.custom.as_ref().is_some_and(|custom| {
                !custom.name.trim().is_empty()
                    && !custom.base_url.trim().is_empty()
                    && !custom.model_id.trim().is_empty()
            })
        {
            self.message = Some(
                "Custom Providerを使うには、名前・接続先URL・モデルIDを入力してください。"
                    .to_owned(),
            );
            return;
        }
        let old_credential = self
            .snapshot
            .settings
            .custom
            .as_ref()
            .and_then(|custom| custom.credential_ref.clone());
        if self.active_connection == ActiveConnection::Custom
            && old_credential.is_none()
            && self.credential_edit == CredentialEdit::Keep
            && !self.needs_durability_reconfirmation()
        {
            self.message = Some(
                "APIキーが未登録です。「登録」から入力し、「保存して適用」を押してください。"
                    .to_owned(),
            );
            return;
        }
        let expected_revision = self.snapshot.settings.revision;
        if self.credential_edit == CredentialEdit::Delete
            && old_credential.is_some()
            && !delete_confirmed
        {
            self.pending_confirmation = Some(PendingConfirmation::DeleteKey(target));
            return;
        }
        if delete_confirmed {
            self.pending_confirmation = None;
        }
        let result = match self.credential_edit {
            CredentialEdit::Replace => {
                let Some(inputs) = &self.inputs else { return };
                let secret = value(&inputs.custom_api_key, cx);
                if secret.is_empty() {
                    self.message = Some("新しいAPIキーを入力してください。".to_owned());
                    return;
                }
                let Some(custom) = proposed.custom.as_mut() else {
                    self.message = Some("先にCustom Providerの設定を入力してください。".to_owned());
                    return;
                };
                custom.credential_ref = old_credential.clone();
                service.try_update_custom_credential(
                    expected_revision,
                    old_credential,
                    proposed,
                    secret,
                )
            }
            CredentialEdit::Delete if old_credential.is_some() => {
                if let Some(custom) = proposed.custom.as_mut() {
                    custom.credential_ref = None;
                }
                service.try_delete_custom_credential(
                    expected_revision,
                    old_credential.expect("checked above"),
                    proposed,
                )
            }
            CredentialEdit::Keep | CredentialEdit::Delete => {
                service.try_save_settings(expected_revision, proposed)
            }
        };
        match result {
            Ok(operation) => {
                if let Some(inputs) = &self.inputs {
                    set_value(&inputs.custom_api_key, "", window, cx);
                }
                self.credential_edit = CredentialEdit::Keep;
                if let Some(target) = target {
                    self.save_then_leave = Some((operation, target));
                }
                self.pending_confirmation = None;
            }
            Err(error) => self.message = Some(admission_message(error)),
        }
    }

    pub(super) fn render(
        &mut self,
        window: &mut Window,
        cx: &mut Context<EditorView>,
        theme: Theme,
    ) -> impl IntoElement {
        self.ensure_inputs(window, cx);
        let durability_reconfirmation = self.needs_durability_reconfirmation();
        let editable = self.settings_editable();
        let ready = self.snapshot.ownership == OwnershipState::Owned
            && (!self.snapshot.recovery_required || durability_reconfirmation)
            && self.inputs.is_some();
        let dirty = self.is_dirty(cx);
        let view = cx.entity();
        let recover = Button::new("ai-settings-retry-open")
            .label(if self.snapshot.recovery_required {
                "設定の復旧を再試行"
            } else {
                "AI状態を再確認"
            })
            .small()
            .on_click(move |_, _, app| {
                view.update(app, |view, cx| view.ai_settings.activate(cx));
            });
        let mut body = div()
            .id("ai-settings-page-content")
            .w_full()
            .max_w(px(948.0))
            .px(px(38.0))
            .py(px(29.0))
            .flex()
            .flex_col()
            .gap_4()
            .text_size(px(12.0))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .text_size(px(27.0))
                                    .font_weight(gpui::FontWeight::BOLD)
                                    .child("AI設定"),
                            )
                            .child(
                                div()
                                    .text_size(px(12.0))
                                    .text_color(rgb(theme.quote_foreground))
                                    .child("使うAIを選び、接続を確認します。"),
                            ),
                    )
                    .child(
                        div()
                            .size(px(40.0))
                            .flex_none()
                            .rounded_sm()
                            .border_1()
                            .border_color(rgb(theme.table_border))
                            .bg(rgb(theme.code_block_background))
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(
                                Icon::new(gpui_kit_assets::IconName::Sparkles)
                                    .size(px(21.0))
                                    .text_color(rgb(theme.link_foreground)),
                            ),
                    )
                    .mb(px(6.0)),
            )
            .child(self.current_connection_section(theme, cx));
        if matches!(
            self.snapshot.ownership,
            OwnershipState::Unknown | OwnershipState::OwnedElsewhere | OwnershipState::Unavailable
        ) || self.snapshot.recovery_required
        {
            body = body.child(recover);
        }
        if ready {
            body = body
                .child(
                    div()
                        .id("ai-settings-draft-heading")
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_between()
                                .gap_3()
                                .child(
                                    div()
                                        .text_size(px(16.0))
                                        .font_weight(gpui::FontWeight::BOLD)
                                        .child("使うAIの設定"),
                                )
                                .child(self.draft_badge(theme, dirty)),
                        )
                        .child(if dirty {
                            if self.active_connection != self.snapshot.settings.active_connection {
                                "接続方法はまだ切り替わっていません。「保存して適用」で反映します。"
                            } else {
                                "変更は「保存して適用」を押した後に有効になります。"
                            }
                        } else if self.snapshot.settings.revision == 0 {
                            "接続方法を選び、必要な項目を設定してください。"
                        } else {
                            "選択した接続方法の設定を確認できます。"
                        }),
                )
                .child(self.connection_section(cx, dirty, editable, theme));
            if self.active_connection == ActiveConnection::ChatGpt {
                body = body.child(self.chatgpt_section(cx, dirty, editable, window, theme));
            } else {
                body = body.child(self.custom_section(cx, dirty, editable, theme));
            }
            body = body.child(self.probe_section(cx, dirty, theme));
            body = body.child(self.diagnostics_toggle_section(theme, cx));
        } else if self.snapshot.ownership == OwnershipState::Owned
            && !self.snapshot.recovery_required
        {
            body = body
                .child(div().child("AI操作の完了を待っています。画面を閉じても処理は続きます。"));
        }
        if let Some(target) = self.leave_prompt {
            body = body.child(self.leave_confirmation(window, cx, target));
        }
        if let Some(message) = &self.message {
            body = body.child(
                div()
                    .id("ai-settings-message")
                    .text_color(rgb(0xb42318))
                    .child(message.clone()),
            );
        }
        if let Some(next) = self.pending_connection_switch {
            body = body.child(self.connection_switch_confirmation(next, window, cx, theme));
        }
        if let Some(confirmation) = self.pending_confirmation {
            body = body.child(self.action_confirmation(confirmation, window, cx, theme));
        }
        let scroll = div()
            .id("ai-settings-scroll-area")
            .flex_1()
            .flex()
            .flex_col()
            .items_center()
            .min_h(px(0.0))
            .overflow_y_scroll()
            .child(body);
        let savebar = ready.then(|| self.save_section(window, cx, dirty, editable, theme));
        div()
            .id("ai-settings-page")
            .size_full()
            .min_h(px(0.0))
            .flex()
            .flex_col()
            .overflow_hidden()
            .child(scroll)
            .children(savebar)
    }

    fn current_connection_section(
        &self,
        theme: Theme,
        cx: &mut Context<EditorView>,
    ) -> impl IntoElement {
        let applied = self.saved_configuration_is_applied();
        let has_saved_settings = self.snapshot.settings.revision > 0;
        let model = match self.snapshot.settings.active_connection {
            ActiveConnection::ChatGpt => self
                .snapshot
                .settings
                .chatgpt
                .model_id
                .as_deref()
                .filter(|model| !model.trim().is_empty())
                .unwrap_or("モデル未選択"),
            ActiveConnection::Custom => self
                .snapshot
                .settings
                .custom
                .as_ref()
                .map(|custom| custom.model_id.as_str())
                .filter(|model| !model.trim().is_empty())
                .unwrap_or("モデル未選択"),
        };
        let provider = match self.snapshot.settings.active_connection {
            ActiveConnection::ChatGpt => "ChatGPT",
            ActiveConnection::Custom => self
                .snapshot
                .settings
                .custom
                .as_ref()
                .map(|custom| custom.name.as_str())
                .filter(|name| !name.trim().is_empty())
                .unwrap_or("接続先未設定"),
        };
        let status_label = if !has_saved_settings {
            "未設定"
        } else if self.snapshot.recovery_required
            || matches!(
                self.snapshot.persistence,
                PersistenceState::CleanupPending
                    | PersistenceState::RecoveryRequired
                    | PersistenceState::DurabilityUnconfirmed
            )
        {
            "設定の復旧が必要"
        } else if self.snapshot.settings.active_connection == ActiveConnection::Custom
            && self
                .snapshot
                .settings
                .custom
                .as_ref()
                .and_then(|custom| custom.credential_ref.as_ref())
                .is_none()
        {
            "APIキー未登録"
        } else if !self.saved_connection_is_configured() {
            "未設定"
        } else if !applied {
            if self.snapshot.runtime_state == hane_ai::RuntimeState::Failed {
                "保存済み・未適用"
            } else {
                "適用状態を確認できません"
            }
        } else if self.snapshot.settings.active_connection == ActiveConnection::ChatGpt
            && !matches!(self.snapshot.account, AccountState::SignedIn { .. })
        {
            "ログインが必要"
        } else {
            match self.snapshot.probe_status {
                ProbeStatus::Succeeded if self.probe_summary().contains("で成功しました。") => {
                    "応答確認済み"
                }
                ProbeStatus::Failed(_) | ProbeStatus::TimedOut | ProbeStatus::Isolated => {
                    "応答確認に失敗"
                }
                ProbeStatus::Running => "応答を確認中",
                _ => "応答は未確認",
            }
        };
        let is_dark = theme.editor_background < 0x888888;
        let (badge_background, badge_foreground) = match status_label {
            "応答確認済み" => {
                if is_dark {
                    (0x223a33, 0xa2ddc5)
                } else {
                    (0xe0f2e8, 0x1c6843)
                }
            }
            "ログインが必要" | "APIキー未登録" | "未設定" | "応答は未確認" => {
                if is_dark {
                    (0x3b3225, 0xf1d094)
                } else {
                    (0xfff0d3, 0x744d0c)
                }
            }
            "応答確認に失敗" | "保存済み・未適用" | "設定の復旧が必要" => {
                if is_dark {
                    (0x412b31, 0xffb2b5)
                } else {
                    (0xfbe4e5, 0x9b2832)
                }
            }
            _ => (theme.sidebar_active_background, theme.foreground),
        };
        let view = cx.entity();
        let retry_apply = Button::new("ai-settings-retry-apply")
            .label("適用を再試行")
            .small()
            .disabled(
                self.snapshot.ownership != OwnershipState::Owned
                    || self.snapshot.busy.is_some()
                    || self.snapshot.recovery_required
                    || self.is_dirty(cx),
            )
            .on_click(move |_, _, app| {
                view.update(app, |view, cx| {
                    if let Some(service) = &view.ai_settings.service
                        && let Err(error) = service.try_submit(AiCommand::Restart)
                    {
                        view.ai_settings.message = Some(admission_message(error));
                        cx.notify();
                    }
                });
            });
        let runtime_needs_retry = !applied
            && self.saved_connection_is_configured()
            && (self.snapshot.settings.active_connection == ActiveConnection::ChatGpt
                || self
                    .snapshot
                    .settings
                    .custom
                    .as_ref()
                    .and_then(|custom| custom.credential_ref.as_ref())
                    .is_some())
            && self.snapshot.ownership == OwnershipState::Owned
            && !self.snapshot.recovery_required;
        let fields = if has_saved_settings {
            let method_label = if applied {
                "接続方法"
            } else {
                "保存済みの接続方法"
            };
            let provider_label = if applied {
                "接続先"
            } else {
                "保存済みの接続先"
            };
            let model_label = if applied {
                "モデル"
            } else {
                "保存済みのモデル"
            };
            div()
                .id("ai-current-values")
                .flex()
                .flex_wrap()
                .gap_4()
                .child(current_value_cell(
                    method_label,
                    connection_name(self.snapshot.settings.active_connection),
                    theme,
                ))
                .child(current_value_cell(provider_label, provider, theme))
                .child(current_value_cell(model_label, model, theme))
                .into_any_element()
        } else {
            div()
                .id("ai-current-empty")
                .flex()
                .flex_col()
                .gap_2()
                .child(
                    div()
                        .text_size(px(17.0))
                        .font_weight(gpui::FontWeight::BOLD)
                        .child("AI接続はまだ設定されていません"),
                )
                .child("下で接続方法を選び、必要な項目を設定してください。")
                .into_any_element()
        };
        let status_message = self.current_connection_message(has_saved_settings, applied);
        let status_icon = match status_label {
            "応答確認済み" => gpui_kit_assets::IconName::CircleCheck,
            "ログインが必要" | "APIキー未登録" | "未設定" => {
                gpui_kit_assets::IconName::CircleAlert
            }
            "応答確認に失敗" | "保存済み・未適用" | "設定の復旧が必要" => {
                gpui_kit_assets::IconName::CircleAlert
            }
            _ => gpui_kit_assets::IconName::Info,
        };
        let dark = theme.editor_background < 0x888888;
        let card_background = if dark {
            0x22252a
        } else {
            theme.code_block_background
        };
        let card_border = if dark { 0x44454f } else { theme.table_border };
        let inner_border = if dark { 0x3a3e46 } else { theme.table_border };
        let status_bottom = has_saved_settings.then(|| {
            let mut bottom = div()
                .mt(px(3.0))
                .pt(px(11.0))
                .border_t_1()
                .border_color(rgb(inner_border))
                .flex()
                .flex_wrap()
                .items_start()
                .justify_between()
                .gap_3()
                .child(
                    div()
                        .flex_1()
                        .min_w(px(220.0))
                        .flex()
                        .items_start()
                        .gap_2()
                        .text_size(px(11.0))
                        .text_color(rgb(theme.quote_foreground))
                        .child(
                            Icon::new(status_icon)
                                .size(px(14.0))
                                .text_color(rgb(badge_foreground)),
                        )
                        .child(status_message),
                );
            if runtime_needs_retry {
                bottom = bottom.child(retry_apply);
            }
            bottom
        });
        let status = div()
            .id("ai-runtime-status")
            .debug_selector(|| "ai-runtime-status".to_owned())
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                Icon::new(gpui_kit_assets::IconName::Check)
                                    .size(px(14.0))
                                    .text_color(rgb(badge_foreground)),
                            )
                            .child(
                                div()
                                    .text_size(px(12.0))
                                    .text_color(rgb(theme.foreground))
                                    .font_weight(gpui::FontWeight::BOLD)
                                    .child("現在有効な設定"),
                            ),
                    )
                    .child(
                        div()
                            .px(px(9.0))
                            .py(px(3.0))
                            .rounded_sm()
                            .bg(rgb(badge_background))
                            .text_color(rgb(badge_foreground))
                            .text_size(px(11.0))
                            .flex()
                            .items_center()
                            .gap_1()
                            .child(div().size(px(5.0)).rounded_full().bg(rgb(badge_foreground)))
                            .child(status_label),
                    ),
            )
            .child(fields)
            .children(status_bottom);
        div()
            .id("ai-current-connection-card")
            .debug_selector(|| "ai-current-connection-card".to_owned())
            .w_full()
            .rounded(px(11.0))
            .border_1()
            .border_color(rgb(card_border))
            .bg(rgb(card_background))
            .flex()
            .overflow_hidden()
            .mb(px(9.0))
            .child(div().w(px(3.0)).flex_none().bg(rgb(theme.link_foreground)))
            .child(
                div()
                    .flex_1()
                    .px(px(20.0))
                    .py(px(17.0))
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(status),
            )
    }

    fn draft_badge(&self, theme: Theme, dirty: bool) -> impl IntoElement {
        let (label, background, foreground) = if dirty {
            (
                "未保存の変更",
                theme.sidebar_active_background,
                theme.sidebar_foreground,
            )
        } else if self.snapshot.settings.revision == 0 {
            ("初回の設定", theme.code_background, theme.foreground)
        } else {
            ("変更なし", theme.code_background, theme.foreground)
        };
        div()
            .id("ai-settings-draft-badge")
            .px(px(9.0))
            .py(px(3.0))
            .rounded_sm()
            .bg(rgb(background))
            .text_color(rgb(foreground))
            .text_size(px(11.0))
            .child(label)
    }

    fn diagnostics_toggle_section(
        &self,
        theme: Theme,
        cx: &mut Context<EditorView>,
    ) -> impl IntoElement {
        let view = cx.entity();
        let toggle = Button::new("ai-diagnostics-toggle")
            .label(if self.diagnostics_open {
                "診断情報を隠す"
            } else {
                "診断情報を表示"
            })
            .small()
            .ghost()
            .on_click(move |_, _, app| {
                view.update(app, |view, cx| {
                    view.ai_settings.diagnostics_open = !view.ai_settings.diagnostics_open;
                    cx.notify();
                });
            });
        let mut section = div()
            .id("ai-diagnostics-toggle-section")
            .w_full()
            .pt(px(10.0))
            .border_t_1()
            .border_color(rgb(theme.table_border))
            .flex()
            .items_center()
            .gap_2()
            .child(toggle)
            .child("接続できないときに確認します。");
        if self.diagnostics_open {
            section = section.child(self.diagnostics_section(theme));
        }
        section
    }

    fn saved_connection_description(&self) -> String {
        match self.snapshot.settings.active_connection {
            ActiveConnection::ChatGpt => {
                let model = self
                    .snapshot
                    .settings
                    .chatgpt
                    .model_id
                    .as_deref()
                    .filter(|model| !model.trim().is_empty());
                match model {
                    Some(model) if self.snapshot.settings.revision > 0 => {
                        format!("ChatGPT / Codex · {model}")
                    }
                    _ => "ChatGPT / Codex · モデル未選択".to_owned(),
                }
            }
            ActiveConnection::Custom => match self.snapshot.settings.custom.as_ref() {
                Some(custom)
                    if self.snapshot.settings.revision > 0
                        && !custom.name.trim().is_empty()
                        && !custom.model_id.trim().is_empty()
                        && !custom.base_url.trim().is_empty() =>
                {
                    format!("APIキー接続 · {} · {}", custom.name, custom.model_id)
                }
                _ => "APIキー接続 · 接続先を設定してください".to_owned(),
            },
        }
    }

    fn saved_connection_is_configured(&self) -> bool {
        if self.snapshot.settings.revision == 0 {
            return false;
        }
        match self.snapshot.settings.active_connection {
            ActiveConnection::ChatGpt => self
                .snapshot
                .settings
                .chatgpt
                .model_id
                .as_deref()
                .is_some_and(|model| !model.trim().is_empty()),
            ActiveConnection::Custom => {
                self.snapshot
                    .settings
                    .custom
                    .as_ref()
                    .is_some_and(|custom| {
                        !custom.name.trim().is_empty()
                            && !custom.base_url.trim().is_empty()
                            && !custom.model_id.trim().is_empty()
                    })
            }
        }
    }

    fn saved_configuration_is_applied(&self) -> bool {
        self.saved_connection_is_configured()
            && self.snapshot.ownership == OwnershipState::Owned
            && self.snapshot.runtime_state == hane_ai::RuntimeState::Ready
            && self.snapshot.configured_settings_generation
                == self.snapshot.settings.settings_generation
            && !self.snapshot.recovery_required
            && matches!(
                self.snapshot.persistence,
                PersistenceState::Clean | PersistenceState::Saved
            )
    }

    fn current_connection_message(&self, has_saved_settings: bool, applied: bool) -> String {
        if !has_saved_settings {
            return "AI接続はまだ設定されていません。下で接続方法を選び、必要な項目を設定してください。"
                .to_owned();
        }
        if self.snapshot.persistence == PersistenceState::NotCommitted
            || self.snapshot.recovery_required
            || matches!(
                self.snapshot.persistence,
                PersistenceState::CleanupPending
                    | PersistenceState::RecoveryRequired
                    | PersistenceState::DurabilityUnconfirmed
            )
        {
            return self.current_next_step(applied);
        }
        if !self.saved_connection_is_configured() || !applied {
            return self.current_next_step(applied);
        }
        if self.snapshot.settings.active_connection == ActiveConnection::ChatGpt {
            if self.snapshot.account_refresh_failed {
                return "ChatGPTのログイン状態を確認できません。状態を更新してください。"
                    .to_owned();
            }
            if !matches!(self.snapshot.account, AccountState::SignedIn { .. }) {
                return self.current_next_step(applied);
            }
        }
        match self.snapshot.probe_status {
            ProbeStatus::Succeeded => self.probe_summary(),
            ProbeStatus::Failed(_) | ProbeStatus::TimedOut | ProbeStatus::Stale => {
                self.probe_summary()
            }
            ProbeStatus::Running => "応答を確認しています。完了をお待ちください。".to_owned(),
            ProbeStatus::Canceled => "直近の応答確認は取り消されました。".to_owned(),
            ProbeStatus::Isolated => self.probe_summary(),
            ProbeStatus::NotRun => {
                "応答はまだ確認していません。必要なら下の「応答を確認」で試せます。".to_owned()
            }
        }
    }

    fn current_next_step(&self, applied: bool) -> String {
        match self.snapshot.persistence {
            PersistenceState::NotCommitted => {
                "設定の保存に失敗しました。入力内容を確認して保存を再試行してください。".to_owned()
            }
            PersistenceState::CleanupPending | PersistenceState::RecoveryRequired => {
                "設定の復旧が必要です。上の「設定の復旧を再試行」を実行してください。".to_owned()
            }
            PersistenceState::DurabilityUnconfirmed => {
                "AI操作は無効です。現在の設定を「設定を再保存して復旧」から書き込み直してください。"
                    .to_owned()
            }
            _ if !self.saved_connection_is_configured() => {
                "次の操作: 接続方式を選び、必要な項目を入力して「保存して適用」を押してください。"
                    .to_owned()
            }
            _ if self.snapshot.settings.active_connection == ActiveConnection::Custom
                && self
                    .snapshot
                    .settings
                    .custom
                    .as_ref()
                    .and_then(|custom| custom.credential_ref.as_ref())
                    .is_none() =>
            {
                "次の操作: APIキーの「登録」を押して入力し、「保存して適用」を押してください。"
                    .to_owned()
            }
            _ if !applied => {
                "保存済み設定の適用を確認できません。「適用を再試行」を押してください。".to_owned()
            }
            _ if self.snapshot.settings.active_connection == ActiveConnection::ChatGpt
                && !matches!(self.snapshot.account, AccountState::SignedIn { .. }) =>
            {
                "次の操作: この接続を使うにはChatGPTにログインしてください。".to_owned()
            }
            _ => "必要なら「応答を確認」を実行してください。設定保存だけでは通信しません。"
                .to_owned(),
        }
    }

    fn probe_summary(&self) -> String {
        match self.snapshot.probe_status {
            ProbeStatus::NotRun => return "応答確認: まだ実行していません。".to_owned(),
            ProbeStatus::Running => return "応答確認: 実行中です。".to_owned(),
            ProbeStatus::Failed(code) => {
                return format!("応答確認: {}", probe_error_message(code));
            }
            ProbeStatus::Canceled => return "応答確認: 取り消されました。".to_owned(),
            ProbeStatus::TimedOut => {
                return "応答確認: 時間切れです。設定と接続先を確認して再試行してください。"
                    .to_owned();
            }
            ProbeStatus::Stale => {
                return "応答確認: 設定変更により、前回結果は古くなっています。".to_owned();
            }
            ProbeStatus::Isolated => {
                return "応答確認: 子プロセスの停止が未確認のため、AI操作を停止しています。"
                    .to_owned();
            }
            ProbeStatus::Succeeded => {}
        }
        let Some(result) = self.snapshot.probe_result.as_ref() else {
            return "応答確認: 成功結果を取得できません。再確認してください。".to_owned();
        };
        let matches = result.connection == self.snapshot.settings.active_connection
            && result.settings_generation == self.snapshot.settings.settings_generation
            && result.runtime_generation == self.snapshot.runtime_generation
            && result.auth_epoch == self.snapshot.auth_epoch
            && match result.connection {
                ActiveConnection::ChatGpt => self
                    .snapshot
                    .settings
                    .chatgpt
                    .model_id
                    .as_deref()
                    .is_some_and(|model| model == result.model),
                ActiveConnection::Custom => self
                    .snapshot
                    .settings
                    .custom
                    .as_ref()
                    .is_some_and(|custom| custom.model_id == result.model),
            };
        if self.snapshot.probe_status == ProbeStatus::Succeeded && matches {
            format!(
                "応答確認: {} / {} で成功しました。",
                connection_name(result.connection),
                result.model
            )
        } else {
            "応答確認: 設定または認証情報の変更により、前回結果は現在の接続には使えません。"
                .to_owned()
        }
    }

    fn diagnostics_section(&self, theme: Theme) -> impl IntoElement {
        let runtime = match self.snapshot.runtime_state {
            hane_ai::RuntimeState::Stopped => "停止中",
            hane_ai::RuntimeState::Starting => "起動中",
            hane_ai::RuntimeState::Initializing => "初期化中",
            hane_ai::RuntimeState::Ready => "準備完了",
            hane_ai::RuntimeState::Stopping => "停止中",
            hane_ai::RuntimeState::Failed => "エラー",
        };
        let ownership = match self.snapshot.ownership {
            OwnershipState::Unknown => "確認中",
            OwnershipState::Owned => "このHaneが管理中",
            OwnershipState::OwnedElsewhere => "別のHaneが管理中",
            OwnershipState::Unavailable => "利用不可",
        };
        div()
            .id("ai-diagnostics-section")
            .w_full()
            .px(px(16.0))
            .py(px(14.0))
            .rounded_sm()
            .border_1()
            .border_color(rgb(theme.table_border))
            .flex()
            .flex_col()
            .gap_1()
            .child("診断情報")
            .child(format!("AI runtime: {runtime} / 所有状態: {ownership}"))
            .child(format!(
                "設定世代: 保存 {} / runtime適用 {} / runtime実行 {}",
                self.snapshot.settings.settings_generation,
                self.snapshot.configured_settings_generation,
                self.snapshot.runtime_generation
            ))
            .child("Codex App Server同梱版と実行中バージョン: 画面からは確認できません")
            .child("秘密のAPI key、token、auth URLは表示・記録しません")
    }

    fn connection_section(
        &mut self,
        cx: &mut Context<EditorView>,
        _dirty: bool,
        editable: bool,
        theme: Theme,
    ) -> impl IntoElement {
        let saved = self.snapshot.settings.active_connection;
        let draft = self.active_connection;
        let applied = self.saved_configuration_is_applied();
        let dark = theme.editor_background < 0x888888;
        let method_border = if dark { 0x373b43 } else { theme.table_border };
        let method_selected_border = if dark {
            0xa997e5
        } else {
            theme.link_foreground
        };
        let method_background = if dark {
            0x1d2025
        } else {
            theme.code_background
        };
        let method_selected_background = if dark {
            0x2b263b
        } else {
            theme.sidebar_active_background
        };
        let view = cx.entity();
        let chatgpt_selected = draft == ActiveConnection::ChatGpt;
        let chatgpt = Radio::new("ai-connection-chatgpt-radio")
            .label("ChatGPTアカウント")
            .accessibility_label("ChatGPTアカウント。ログインして接続します。")
            .checked(chatgpt_selected)
            .disabled(!editable)
            .on_click(move |_, window, app| {
                view.update(app, |view, cx| {
                    view.ai_settings.request_connection_switch(
                        ActiveConnection::ChatGpt,
                        window,
                        cx,
                    );
                    cx.notify();
                })
            })
            .flex_1()
            .min_w(px(250.0))
            .min_h(px(91.0))
            .px(px(14.0))
            .py(px(14.0))
            .rounded(px(9.0))
            .border_1()
            .border_color(rgb(if chatgpt_selected {
                method_selected_border
            } else {
                method_border
            }))
            .bg(rgb(if chatgpt_selected {
                method_selected_background
            } else {
                method_background
            }))
            .child(
                div()
                    .text_size(px(11.0))
                    .text_color(rgb(theme.quote_foreground))
                    .child("ログインして接続します。"),
            )
            .children(
                (applied && saved == ActiveConnection::ChatGpt).then_some(
                    div()
                        .text_size(px(10.0))
                        .text_color(rgb(theme.link_foreground))
                        .child("現在の接続方法"),
                ),
            );
        let view = cx.entity();
        let custom_selected = draft == ActiveConnection::Custom;
        let custom = Radio::new("ai-connection-custom-radio")
            .label("APIキー")
            .accessibility_label("APIキー。AIサービスのキーで接続します。")
            .checked(custom_selected)
            .disabled(!editable)
            .on_click(move |_, window, app| {
                view.update(app, |view, cx| {
                    view.ai_settings.request_connection_switch(
                        ActiveConnection::Custom,
                        window,
                        cx,
                    );
                    cx.notify();
                })
            })
            .flex_1()
            .min_w(px(250.0))
            .min_h(px(91.0))
            .px(px(14.0))
            .py(px(14.0))
            .rounded(px(9.0))
            .border_1()
            .border_color(rgb(if custom_selected {
                method_selected_border
            } else {
                method_border
            }))
            .bg(rgb(if custom_selected {
                method_selected_background
            } else {
                method_background
            }))
            .child(
                div()
                    .text_size(px(11.0))
                    .text_color(rgb(theme.quote_foreground))
                    .child("AIサービスのキーで接続します。"),
            )
            .children(
                (applied && saved == ActiveConnection::Custom).then_some(
                    div()
                        .text_size(px(10.0))
                        .text_color(rgb(theme.link_foreground))
                        .child("現在の接続方法"),
                ),
            );
        div()
            .id("ai-connection-section")
            .debug_selector(|| "ai-connection-section".to_owned())
            .w_full()
            .flex()
            .flex_wrap()
            .gap_3()
            .child(
                div()
                    .id("ai-connection-chatgpt-wrapper")
                    .debug_selector(|| "ai-connection-chatgpt".to_owned())
                    .flex_1()
                    .min_w(px(250.0))
                    .child(chatgpt),
            )
            .child(
                div()
                    .id("ai-connection-custom-wrapper")
                    .debug_selector(|| "ai-connection-custom".to_owned())
                    .flex_1()
                    .min_w(px(250.0))
                    .child(custom),
            )
    }

    fn connection_switch_confirmation(
        &self,
        next: ActiveConnection,
        window: &mut Window,
        cx: &mut Context<EditorView>,
        theme: Theme,
    ) -> impl IntoElement {
        let current = self.active_connection;
        let view = cx.entity();
        let discard = Button::new("ai-connection-switch-discard")
            .label("入力を破棄して切り替える")
            .on_click(move |_, window, app| {
                view.update(app, |view, cx| {
                    view.ai_settings
                        .discard_connection_draft(current, window, cx);
                    view.ai_settings.active_connection = next;
                    cx.notify();
                });
            });
        let view = cx.entity();
        let keep = Button::new("ai-connection-switch-cancel")
            .label("編集を続ける")
            .ghost()
            .on_click(move |_, _, app| {
                view.update(app, |view, cx| {
                    view.ai_settings.pending_connection_switch = None;
                    cx.notify();
                });
            });
        let _ = window;
        div()
            .id("ai-connection-switch-confirmation")
            .w_full()
            .px(px(16.0))
            .py(px(14.0))
            .rounded_sm()
            .border_1()
            .border_color(rgb(theme.table_border))
            .flex()
            .flex_col()
            .gap_2()
            .child("この接続で未保存の入力があります。破棄して切り替えますか？保存済みの接続情報とログイン状態は残ります。")
            .child(div().flex().gap_2().child(discard).child(keep))
    }

    fn action_confirmation(
        &self,
        confirmation: PendingConfirmation,
        window: &mut Window,
        cx: &mut Context<EditorView>,
        theme: Theme,
    ) -> impl IntoElement {
        let (title, message, confirm_label) = match confirmation {
            PendingConfirmation::Logout => (
                "ChatGPTからログアウトしますか？",
                if self.snapshot.settings.active_connection == ActiveConnection::ChatGpt {
                    "現在の有効な接続はChatGPT / Codexです。ログアウト後は、再度ログインするまでこの接続を使えません。変更を破棄してもログイン状態は戻りません。"
                } else {
                    "現在のAPIキー接続には影響しません。ChatGPT / Codexを使うときは再度ログインが必要です。"
                },
                "ログアウト",
            ),
            PendingConfirmation::DeleteKey(_) => (
                "APIキーを削除しますか？",
                "保存して適用すると登録済みキーを削除します。この接続はキーを再登録するまで利用できません。",
                "削除して適用",
            ),
        };
        let view = cx.entity();
        let confirm = Button::new("ai-confirmation-confirm")
            .label(confirm_label)
            .on_click(move |_, window, app| {
                view.update(app, |view, cx| {
                    match confirmation {
                        PendingConfirmation::Logout => {
                            view.ai_settings.pending_confirmation = None;
                            if let Some(service) = &view.ai_settings.service
                                && let Err(error) = service.try_submit(AiCommand::Logout)
                            {
                                view.ai_settings.message = Some(admission_message(error));
                            }
                        }
                        PendingConfirmation::DeleteKey(target) => {
                            view.ai_settings.save_confirmed(window, cx, target);
                        }
                    }
                    cx.notify();
                });
            });
        let view = cx.entity();
        let cancel = Button::new("ai-confirmation-cancel")
            .label("やめる")
            .ghost()
            .on_click(move |_, _, app| {
                view.update(app, |view, cx| {
                    view.ai_settings.pending_confirmation = None;
                    cx.notify();
                });
            });
        let _ = window;
        div()
            .id("ai-settings-confirmation")
            .w_full()
            .px(px(16.0))
            .py(px(14.0))
            .rounded_sm()
            .border_1()
            .border_color(rgb(theme.table_border))
            .flex()
            .flex_col()
            .gap_2()
            .child(div().font_weight(gpui::FontWeight::BOLD).child(title))
            .child(message)
            .child(div().flex().gap_2().child(confirm).child(cancel))
    }

    #[inline(never)]
    fn chatgpt_section(
        &self,
        cx: &mut Context<EditorView>,
        dirty: bool,
        editable: bool,
        _window: &Window,
        theme: Theme,
    ) -> gpui::AnyElement {
        let actions_allowed = self.snapshot.settings.active_connection == ActiveConnection::ChatGpt
            && !dirty
            && self.snapshot.busy.is_none()
            && !self.snapshot.recovery_required;
        let is_signed_in = matches!(self.snapshot.account, AccountState::SignedIn { .. });
        let account = self.chatgpt_account_content(cx, dirty, theme);
        let models =
            self.chatgpt_model_controls(cx, is_signed_in, editable, actions_allowed, theme);
        let dark = theme.editor_background < 0x888888;
        div()
            .id("ai-chatgpt-section")
            .debug_selector(|| "ai-chatgpt-section".to_owned())
            .w_full()
            .px(px(20.0))
            .py(px(18.0))
            .rounded(px(9.0))
            .border_1()
            .border_color(rgb(if dark { 0x373b43 } else { theme.table_border }))
            .bg(rgb(if dark {
                0x202328
            } else {
                theme.code_block_background
            }))
            .text_size(px(12.0))
            .flex()
            .flex_col()
            .gap_3()
            .child(account)
            .child(models)
            .into_any_element()
    }

    #[inline(never)]
    fn chatgpt_account_content(
        &self,
        cx: &mut Context<EditorView>,
        dirty: bool,
        theme: Theme,
    ) -> gpui::AnyElement {
        if self.inputs.is_none() {
            return div().into_any_element();
        }
        let active_saved = self.snapshot.settings.active_connection == ActiveConnection::ChatGpt;
        let actions_allowed = active_saved
            && !dirty
            && self.snapshot.busy.is_none()
            && !self.snapshot.recovery_required;
        let dark = theme.editor_background < 0x888888;
        let account_background = if dark {
            0x1d2025
        } else {
            theme.code_background
        };
        let signed_in_email = match &self.snapshot.account {
            AccountState::SignedIn { email, .. } => email.as_deref(),
            _ => None,
        };
        let is_signed_in = signed_in_email.is_some()
            || matches!(self.snapshot.account, AccountState::SignedIn { .. });
        let account = match &self.snapshot.account {
            AccountState::Unknown => "未確認",
            AccountState::SignedOut => "未ログイン",
            AccountState::SignedIn { .. } => "ログイン済み",
            AccountState::ApiKey => "OAuthログイン状態を確認できません",
        };
        let account_title = if is_signed_in {
            signed_in_email.unwrap_or("ChatGPTにログイン済み")
        } else {
            "ChatGPTにログインしてください"
        };
        let account_subtitle = if is_signed_in {
            "ChatGPTアカウントで利用するモデルを選びます。"
        } else {
            "ログインして、利用するモデルを選びます。"
        };
        let service = self.service.clone();
        let view_for_login = cx.entity();
        let login_label = match self.snapshot.login {
            LoginState::Starting => "ログインを開始しています…",
            LoginState::AwaitingBrowser => "ブラウザーでログイン中…",
            LoginState::Reconciling => "ログイン状態を確認しています…",
            LoginState::Canceling => "ログインを取り消しています…",
            LoginState::Idle | LoginState::Finished => "ChatGPTにログイン",
        };
        let login = Button::new("ai-chatgpt-login")
            .label(login_label)
            .disabled(
                !actions_allowed || matches!(self.snapshot.account, AccountState::SignedIn { .. }),
            )
            .on_click(move |_, _, app| {
                if let Some(service) = &service
                    && let Err(error) = service.try_submit(AiCommand::StartLogin)
                {
                    view_for_login.update(app, |view, cx| {
                        view.ai_settings.message = Some(admission_message(error));
                        cx.notify();
                    });
                }
            });
        let cancel_login = if let Some((id, ServiceBusyReason::Login)) = self.snapshot.busy {
            let service = self.service.clone();
            let view = cx.entity();
            Some(
                Button::new("ai-chatgpt-cancel-login")
                    .label("ログインを取り消す")
                    .on_click(move |_, _, app| {
                        if let Some(service) = &service
                            && let Err(error) = service.cancel(id)
                        {
                            view.update(app, |view, cx| {
                                view.ai_settings.message = Some(admission_message(error));
                                cx.notify();
                            });
                        }
                    }),
            )
        } else {
            None
        };
        let service = self.service.clone();
        let view = cx.entity();
        let refresh_account = Button::new("ai-chatgpt-refresh-account")
            .label("状態を更新")
            .small()
            .disabled(!actions_allowed)
            .on_click(move |_, _, app| {
                if let Some(service) = &service
                    && let Err(error) = service.try_submit(AiCommand::RefreshAccount {
                        refresh_token: false,
                    })
                {
                    view.update(app, |view, cx| {
                        view.ai_settings.message = Some(admission_message(error));
                        cx.notify();
                    });
                }
            });
        let view = cx.entity();
        let logout = Button::new("ai-chatgpt-logout")
            .label("ログアウト")
            .small()
            .disabled(
                !actions_allowed || !matches!(self.snapshot.account, AccountState::SignedIn { .. }),
            )
            .on_click(move |_, _, app| {
                view.update(app, |view, cx| {
                    view.ai_settings.pending_confirmation = Some(PendingConfirmation::Logout);
                    view.ai_settings.message = None;
                    cx.notify();
                });
            });
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .child(
                        div()
                            .text_size(px(13.0))
                            .font_weight(gpui::FontWeight::BOLD)
                            .child("ChatGPTアカウントで接続"),
                    )
                    .child(
                        div()
                            .px(px(8.0))
                            .py(px(3.0))
                            .rounded_sm()
                            .bg(rgb(theme.sidebar_active_background))
                            .text_size(px(11.0))
                            .child(account),
                    ),
            )
            .child(
                div()
                    .id("ai-chatgpt-account-card")
                    .px(px(14.0))
                    .py(px(12.0))
                    .rounded(px(9.0))
                    .bg(rgb(account_background))
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(
                        div()
                            .size(px(38.0))
                            .flex_none()
                            .rounded_sm()
                            .border_1()
                            .border_color(rgb(if theme.editor_background < 0x888888 {
                                0x514d61
                            } else {
                                theme.table_border
                            }))
                            .bg(rgb(theme.sidebar_active_background))
                            .text_color(rgb(theme.link_foreground))
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(Icon::new(gpui_kit_assets::IconName::UserRound).size(px(18.0))),
                    )
                    .child(
                        div()
                            .id("ai-chatgpt-account-copy")
                            .flex_1()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .font_weight(gpui::FontWeight::MEDIUM)
                                    .child(account_title.to_owned()),
                            )
                            .child(
                                div()
                                    .text_size(px(11.0))
                                    .text_color(rgb(theme.quote_foreground))
                                    .child(account_subtitle),
                            ),
                    )
                    .children(is_signed_in.then_some(logout)),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .gap_2()
                            .children((!is_signed_in).then_some(login))
                            .children(cancel_login)
                            .child(refresh_account),
                    )
                    .child(if active_saved {
                        "ログイン後、モデルを選びます。ログインだけでは応答確認を行いません。"
                    } else {
                        "ChatGPTを使うには、先に選んで「保存して適用」します。"
                    }),
            )
            .into_any_element()
    }

    // Keep the model form out of the account-card stack frame. In debug builds,
    // GPUI's large by-value style builders otherwise exhaust Windows' 1 MiB stack.
    #[inline(never)]
    fn chatgpt_model_controls(
        &self,
        cx: &mut Context<EditorView>,
        is_signed_in: bool,
        editable: bool,
        actions_allowed: bool,
        theme: Theme,
    ) -> gpui::AnyElement {
        let Some(inputs) = self.inputs.as_ref() else {
            return div().into_any_element();
        };
        let dark = theme.editor_background < 0x888888;
        let model_list = match &self.snapshot.model_list {
            ModelListState::NotLoaded => {
                "モデル一覧は未取得です。「モデル一覧を更新」から取得できます。".to_owned()
            }
            ModelListState::Loading => "モデル一覧を取得しています…".to_owned(),
            ModelListState::Loaded(models) => format!("{}件のモデルを取得しました。", models.len()),
            ModelListState::Failed(error) => format!(
                "モデル一覧を取得できませんでした（{}）。保存済みモデルは変更していません。再取得するか、下のモデルIDを手入力してください。",
                model_list_error_message(error)
            ),
        };
        let draft_model = value(&inputs.chatgpt_model, cx);
        let model_items = match &self.snapshot.model_list {
            ModelListState::Loaded(available) => available
                .iter()
                .take(40)
                .map(|model| (model.model.clone(), model.display_name.clone()))
                .collect::<Vec<_>>(),
            _ => Vec::new(),
        };
        let model_items_available = !model_items.is_empty();
        let selected_model_label = if draft_model.trim().is_empty() {
            "モデルを選択".to_owned()
        } else {
            draft_model.clone()
        };
        let view_for_models = cx.entity();
        let model_picker = Button::new("ai-chatgpt-model-picker")
            .label(selected_model_label)
            .disabled(!editable || !model_items_available)
            .dropdown_menu(move |mut menu, _, _| {
                for (model, display_name) in &model_items {
                    let model = model.clone();
                    let label = format!("{display_name} ({model})");
                    let item_id = format!("ai-model-{}", models_hash(&model));
                    let selector = item_id;
                    let current_model = draft_model.clone();
                    let view = view_for_models.clone();
                    menu = menu.item(
                        PopupMenuItem::element(move |_, _| {
                            let selector = selector.clone();
                            div()
                                .id(selector.clone())
                                .debug_selector(move || selector.clone())
                                .px(px(5.0))
                                .py(px(3.0))
                                .child(label.clone())
                        })
                        .checked(model == current_model)
                        .on_click(move |_, window, app| {
                            view.update(app, |view, cx| {
                                if let Some(inputs) = &view.ai_settings.inputs {
                                    inputs.chatgpt_model.update(cx, |state, cx| {
                                        state.set_value(model.clone(), window, cx)
                                    });
                                }
                                cx.notify();
                            });
                        }),
                    );
                }
                menu
            });
        let model_picker = div()
            .id("ai-chatgpt-model-picker-wrapper")
            .debug_selector(|| "ai-chatgpt-model-picker".to_owned())
            .child(model_picker);
        let service = self.service.clone();
        let view = cx.entity();
        let refresh_models = Button::new("ai-chatgpt-refresh-models")
            .label("モデル一覧を更新")
            .small()
            .disabled(!actions_allowed)
            .on_click(move |_, _, app| {
                if let Some(service) = &service
                    && let Err(error) = service.try_submit(AiCommand::RefreshModels)
                {
                    view.update(app, |view, cx| {
                        view.ai_settings.message = Some(admission_message(error));
                        cx.notify();
                    });
                }
            });
        let model_available = matches!(self.snapshot.model_list, ModelListState::Loaded(_));
        let saved_model = self
            .snapshot
            .settings
            .chatgpt
            .model_id
            .as_deref()
            .unwrap_or_default();
        let model_stale = model_available
            && !matches!(&self.snapshot.model_list,
            ModelListState::Loaded(models) if models.iter().any(|model| model.model == saved_model));
        if is_signed_in {
            div()
                .id("ai-chatgpt-model-section")
                .debug_selector(|| "ai-chatgpt-model-section".to_owned())
                .flex()
                .flex_col()
                .gap_2()
                .pt(px(16.0))
                .border_t_1()
                .border_color(rgb(if dark { 0x363b43 } else { theme.table_border }))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_3()
                        .child(div().font_weight(gpui::FontWeight::BOLD).child("モデル"))
                        .child(refresh_models),
                )
                .child(model_list)
                .child(model_picker)
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child("モデルID（一覧にないモデルを使う場合）")
                        .child(
                            Input::new(&inputs.chatgpt_model)
                                .aria_label("ChatGPT model ID")
                                .disabled(
                                    !editable_field(
                                        self.snapshot.ownership,
                                        self.snapshot.recovery_required,
                                    ) || self.snapshot.busy.is_some(),
                                ),
                        ),
                )
                .children(model_stale.then_some(
                    div().text_color(rgb(0xb54708)).child(
                        "保存済みモデルは一覧で見つかりません。自動変更していません。IDを確認するか、一覧を更新してください。",
                    ),
                ))
                .child("モデル一覧から選ぶか、モデルIDを手入力できます。")
                .into_any_element()
        } else {
            div()
                .id("ai-chatgpt-model-lock")
                .debug_selector(|| "ai-chatgpt-model-lock".to_owned())
                .flex()
                .items_center()
                .gap_2()
                .pt(px(15.0))
                .border_t_1()
                .border_color(rgb(if theme.editor_background < 0x888888 {
                    0x363b43
                } else {
                    theme.table_border
                }))
                .text_size(px(11.0))
                .text_color(rgb(theme.quote_foreground))
                .child(
                    Icon::new(gpui_kit_assets::IconName::Lock)
                        .size(px(14.0))
                        .text_color(rgb(theme.quote_foreground)),
                )
                .child("ログインすると、利用するモデルを選べます。")
                .into_any_element()
        }
    }

    fn custom_section(
        &mut self,
        cx: &mut Context<EditorView>,
        _dirty: bool,
        editable: bool,
        theme: Theme,
    ) -> gpui::AnyElement {
        let Some(inputs) = &self.inputs else {
            return div().into_any_element();
        };
        let dark = theme.editor_background < 0x888888;
        let form_background = if dark {
            0x202328
        } else {
            theme.code_block_background
        };
        let form_border = if dark { 0x373b43 } else { theme.table_border };
        let divider = if dark { 0x363b43 } else { theme.table_border };
        let custom = self.snapshot.settings.custom.as_ref();
        let registered = custom
            .and_then(|custom| custom.credential_ref.as_ref())
            .is_some();
        let disabled = !editable || self.snapshot.busy.is_some();
        let (_, draft_base_url, _) = self.custom_draft_values(inputs, cx);
        let endpoint_changed =
            endpoint_changed_with_registered_key(custom, &draft_base_url, self.credential_edit);
        let endpoint_error = if self.custom_preset == CustomProviderPreset::Other
            && !draft_base_url.trim().is_empty()
        {
            hane_ai::validate_base_url(&draft_base_url)
                .err()
                .map(base_url_validation_message)
        } else {
            None
        };

        let service_picker = self.custom_service_picker(cx, disabled);

        let (key_row, key_input) =
            self.custom_key_content(cx, disabled, registered, divider, theme);

        let details_open = self.custom_details_open;
        let view = cx.entity();
        let details_toggle_button = Button::new("ai-custom-details-toggle-button")
            .label(if details_open {
                "接続先の詳細を隠す"
            } else if self.custom_preset == CustomProviderPreset::Other {
                "接続先の詳細（接続名・URLが必要です）"
            } else {
                "接続先の詳細"
            })
            .small()
            .ghost()
            .disabled(disabled)
            .on_click(move |_, _, app| {
                view.update(app, |view, cx| {
                    view.ai_settings.custom_details_open = !view.ai_settings.custom_details_open;
                    cx.notify();
                });
            });
        let details_toggle = div()
            .id("ai-custom-details-toggle-wrapper")
            .debug_selector(|| "ai-custom-details-toggle".to_owned())
            .pt(px(11.0))
            .border_t_1()
            .border_color(rgb(divider))
            .child(details_toggle_button);
        let provider_details = self.provider_details(disabled, endpoint_error);
        let service_model = self.service_model(service_picker, disabled);

        div()
            .id("ai-custom-section")
            .debug_selector(|| "ai-custom-section".to_owned())
            .w_full()
            .px(px(20.0))
            .py(px(18.0))
            .rounded(px(9.0))
            .border_1()
            .border_color(rgb(form_border))
            .bg(rgb(form_background))
            .text_size(px(12.0))
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .child(
                        div()
                            .text_size(px(13.0))
                            .font_weight(gpui::FontWeight::BOLD)
                            .child("APIキーで接続"),
                    )
                    .child(
                        div()
                            .text_size(px(10.0))
                            .text_color(rgb(theme.quote_foreground))
                            .child("サービス側のAPI利用料金がかかる場合があります。"),
                    ),
            )
            .child(service_model)
            .child(key_row)
            .children(key_input)
            .children(endpoint_changed.then_some(
                div()
                    .id("ai-custom-key-endpoint-changed")
                    .text_color(rgb(0xb54708))
                    .child("接続先が変わりました。この接続先に合うAPIキーを登録してください。登録済みキーは別の接続先へ自動では引き継ぎません。"),
            ))
            .child(details_toggle)
            .child(provider_details)
            .child(
                div()
                    .text_size(px(10.0))
                    .text_color(rgb(theme.quote_foreground))
                    .child(custom_key_guidance(self.credential_edit, registered)),
            )
            .into_any_element()
    }

    #[inline(never)]
    fn custom_service_picker(
        &self,
        cx: &mut Context<EditorView>,
        disabled: bool,
    ) -> gpui::AnyElement {
        let service_is_openai = self.custom_preset == CustomProviderPreset::OpenAi;
        let openai_view = cx.entity();
        let other_view = cx.entity();
        let service_picker = Button::new("ai-custom-service-picker")
            .label(if service_is_openai {
                "OpenAI"
            } else {
                "その他の接続先"
            })
            .disabled(disabled)
            .dropdown_menu(move |mut menu, _, _| {
                let view = openai_view.clone();
                menu = menu.item(
                    PopupMenuItem::element(move |_, _| {
                        div()
                            .id("ai-custom-service-openai")
                            .debug_selector(|| "ai-custom-service-openai".to_owned())
                            .px(px(5.0))
                            .py(px(3.0))
                            .child("OpenAI")
                    })
                    .checked(service_is_openai)
                    .on_click(move |_, _, app| {
                        view.update(app, |view, cx| {
                            view.ai_settings.custom_preset = CustomProviderPreset::OpenAi;
                            view.ai_settings.custom_details_open = false;
                            cx.notify();
                        });
                    }),
                );
                let view = other_view.clone();
                menu.item(
                    PopupMenuItem::element(move |_, _| {
                        div()
                            .id("ai-custom-service-other")
                            .debug_selector(|| "ai-custom-service-other".to_owned())
                            .px(px(5.0))
                            .py(px(3.0))
                            .child("その他の接続先")
                    })
                    .checked(!service_is_openai)
                    .on_click(move |_, window, app| {
                        view.update(app, |view, cx| {
                            let saved = view.ai_settings.snapshot.settings.custom.as_ref();
                            if let Some(saved) = saved
                                && custom_settings_match_openai(&saved.name, &saved.base_url)
                                && let Some(inputs) = &view.ai_settings.inputs
                            {
                                set_value(&inputs.custom_name, &saved.name, window, cx);
                                set_value(&inputs.custom_base_url, &saved.base_url, window, cx);
                            }
                            view.ai_settings.custom_preset = CustomProviderPreset::Other;
                            view.ai_settings.custom_details_open = true;
                            cx.notify();
                        });
                    }),
                )
            });

        service_picker.into_any_element()
    }

    #[inline(never)]
    fn custom_key_content(
        &self,
        cx: &mut Context<EditorView>,
        disabled: bool,
        registered: bool,
        divider: u32,
        theme: Theme,
    ) -> (gpui::AnyElement, Option<gpui::AnyElement>) {
        let Some(inputs) = self.inputs.as_ref() else {
            return (div().into_any_element(), None);
        };
        let view = cx.entity();
        let replace_key = Button::new("ai-custom-key-replace")
            .label(custom_key_action_label(registered))
            .small()
            .disabled(disabled || self.credential_edit == CredentialEdit::Delete)
            .on_click(move |_, _, app| {
                view.update(app, |view, cx| {
                    view.ai_settings.credential_edit = CredentialEdit::Replace;
                    cx.notify();
                })
            });
        let replace_key = div()
            .id("ai-custom-key-replace-wrapper")
            .debug_selector(|| "ai-custom-key-replace".to_owned())
            .child(replace_key);
        let view = cx.entity();
        let delete_key = if registered && self.credential_edit != CredentialEdit::Delete {
            Some(
                Button::new("ai-custom-key-delete")
                    .label("削除")
                    .small()
                    .disabled(disabled)
                    .on_click(move |_, window, app| {
                        view.update(app, |view, cx| {
                            view.ai_settings.credential_edit = CredentialEdit::Delete;
                            if let Some(input) = &view.ai_settings.inputs {
                                set_value(&input.custom_api_key, "", window, cx);
                            }
                            cx.notify();
                        });
                    }),
            )
        } else {
            None
        };
        let view = cx.entity();
        let undo_delete = if self.credential_edit == CredentialEdit::Delete {
            Some(
                Button::new("ai-custom-key-undo-delete")
                    .label("元に戻す")
                    .small()
                    .disabled(disabled)
                    .on_click(move |_, window, app| {
                        view.update(app, |view, cx| {
                            view.ai_settings.credential_edit = CredentialEdit::Keep;
                            if let Some(input) = &view.ai_settings.inputs {
                                set_value(&input.custom_api_key, "", window, cx);
                            }
                            cx.notify();
                        });
                    }),
            )
        } else {
            None
        };
        let key_status = if self.credential_edit == CredentialEdit::Delete {
            "削除予定"
        } else if self.credential_edit == CredentialEdit::Replace {
            "新しいキーを入力中"
        } else if registered {
            "登録済み"
        } else {
            "未登録"
        };
        let key_input = if self.credential_edit == CredentialEdit::Replace {
            let view = cx.entity();
            let cancel = div()
                .id("ai-custom-key-cancel-wrapper")
                .debug_selector(|| "ai-custom-key-cancel".to_owned())
                .child(
                    Button::new("ai-custom-key-cancel")
                        .label("やめる")
                        .small()
                        .ghost()
                        .disabled(disabled)
                        .on_click(move |_, window, app| {
                            view.update(app, |view, cx| {
                                view.ai_settings.credential_edit = CredentialEdit::Keep;
                                if let Some(input) = &view.ai_settings.inputs {
                                    set_value(&input.custom_api_key, "", window, cx);
                                }
                                cx.notify();
                            });
                        }),
                );
            Some(
                div()
                    .id("ai-custom-api-key-input")
                    .debug_selector(|| "ai-custom-api-key-input".to_owned())
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap_2()
                            .child("新しいAPIキー")
                            .child(cancel),
                    )
                    .child(
                        Input::new(&inputs.custom_api_key)
                            .aria_label("Custom Provider APIキー")
                            .content_type(InputContentType::NewPassword)
                            .disabled(disabled),
                    )
                    .child("キーは伏字で入力され、保存後は画面に表示しません。"),
            )
        } else {
            None
        };

        let key_actions = div()
            .flex()
            .flex_wrap()
            .gap_2()
            .children((self.credential_edit != CredentialEdit::Replace).then_some(replace_key))
            .children(delete_key)
            .children(undo_delete);
        let key_row = self.key_row(key_actions, key_status, registered, divider, theme);
        (key_row, key_input.map(IntoElement::into_any_element))
    }

    #[inline(never)]
    fn provider_details(
        &self,
        disabled: bool,
        endpoint_error: Option<&'static str>,
    ) -> gpui::AnyElement {
        let Some(inputs) = self.inputs.as_ref() else {
            return div().into_any_element();
        };
        let details_open = self.custom_details_open;
        let provider_details = if details_open {
            if self.custom_preset == CustomProviderPreset::OpenAi {
                div()
                    .id("ai-custom-openai-details")
                    .debug_selector(|| "ai-custom-openai-details".to_owned())
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child("接続先URL")
                    .child(OPENAI_PROVIDER_URL)
                    .into_any_element()
            } else {
                div()
                    .id("ai-custom-provider-details")
                    .debug_selector(|| "ai-custom-provider-details".to_owned())
                    .flex()
                    .flex_wrap()
                    .gap_3()
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(250.0))
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child("接続名")
                            .child(
                                Input::new(&inputs.custom_name)
                                    .aria_label("Provider name")
                                    .disabled(disabled),
                            ),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(250.0))
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child("接続先URL")
                            .child(
                                Input::new(&inputs.custom_base_url)
                                    .aria_label("Provider base URL")
                                    .disabled(disabled),
                            )
                            .child("HTTPSを使用してください。HTTPはlocalhost・127.0.0.1・::1での開発に限られます。URLに認証情報を含めないでください.")
                            .children(endpoint_error.map(|message| {
                                div()
                                    .id("ai-custom-base-url-error")
                                    .text_color(rgb(0xb42318))
                                    .child(message)
                            })),
                    )
                    .into_any_element()
            }
        } else {
            div().into_any_element()
        };

        provider_details.into_any_element()
    }

    #[inline(never)]
    fn service_model(&self, service_picker: impl IntoElement, disabled: bool) -> gpui::AnyElement {
        let Some(inputs) = self.inputs.as_ref() else {
            return div().into_any_element();
        };
        let service_model = div()
            .flex()
            .flex_wrap()
            .gap_3()
            .child(
                div()
                    .id("ai-custom-service-choice")
                    .flex_1()
                    .min_w(px(250.0))
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child("AIサービス")
                    .child(service_picker)
                    .child(if self.custom_preset == CustomProviderPreset::OpenAi {
                        "OpenAIの接続先を自動で設定します。"
                    } else {
                        "接続名とURLは「接続先の詳細」で指定します。"
                    }),
            )
            .child(
                div()
                    .id("ai-custom-model-field")
                    .flex_1()
                    .min_w(px(250.0))
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child("モデルID")
                    .child(
                        Input::new(&inputs.custom_model)
                            .aria_label("Custom model ID")
                            .disabled(disabled),
                    )
                    .child("利用するモデルIDを入力してください。"),
            );
        service_model.into_any_element()
    }

    #[inline(never)]
    fn key_row(
        &self,
        key_actions: impl IntoElement,
        key_status: &'static str,
        registered: bool,
        divider: u32,
        theme: Theme,
    ) -> gpui::AnyElement {
        let key_row = div()
            .id("ai-custom-key-row")
            .mt(px(17.0))
            .pt(px(15.0))
            .border_t_1()
            .border_color(rgb(divider))
            .flex()
            .flex_wrap()
            .items_center()
            .justify_between()
            .gap_3()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(div().font_weight(gpui::FontWeight::BOLD).child("APIキー"))
                            .child(
                                div()
                                    .px(px(8.0))
                                    .py(px(2.0))
                                    .rounded_sm()
                                    .bg(rgb(theme.sidebar_active_background))
                                    .text_size(px(11.0))
                                    .child(key_status),
                            ),
                    )
                    .child(if registered {
                        "登録済みのキーは表示しません。"
                    } else {
                        "この接続先で使うAPIキーを登録してください。"
                    }),
            )
            .child(key_actions);

        key_row.into_any_element()
    }

    fn probe_section(
        &self,
        cx: &mut Context<EditorView>,
        dirty: bool,
        theme: Theme,
    ) -> impl IntoElement {
        let result = self.probe_summary();
        let probe_disabled_reason = self.probe_disabled_reason(dirty);
        let probe_enabled = probe_disabled_reason.is_none();
        let target = if self.saved_configuration_is_applied() {
            self.saved_connection_description()
        } else {
            "保存済み設定は適用状態を確認できません".to_owned()
        };
        let (action, action_selector) =
            if let Some((operation, ServiceBusyReason::Probe)) = self.snapshot.busy {
                let service = self.service.clone();
                let view = cx.entity();
                (
                    Button::new("ai-probe-cancel")
                        .label("接続確認を取り消す")
                        .on_click(move |_, _, app| {
                            if let Some(service) = &service
                                && let Err(error) = service.cancel(operation)
                            {
                                view.update(app, |view, cx| {
                                    view.ai_settings.message = Some(admission_message(error));
                                    cx.notify();
                                });
                            }
                        }),
                    "ai-probe-cancel",
                )
            } else {
                let service = self.service.clone();
                let view = cx.entity();
                (
                    Button::new("ai-probe-run")
                        .label("応答を確認")
                        .disabled(!probe_enabled)
                        .on_click(move |_, _, app| {
                            if let Some(service) = &service
                                && let Err(error) = service.try_submit(AiCommand::Probe)
                            {
                                view.update(app, |view, cx| {
                                    view.ai_settings.message = Some(admission_message(error));
                                    cx.notify();
                                });
                            }
                        }),
                    "ai-probe-run",
                )
            };
        let action = div()
            .id(format!("{action_selector}-wrapper"))
            .debug_selector(|| action_selector.to_owned())
            .child(action);
        let result_state =
            (!matches!(&self.snapshot.probe_status, ProbeStatus::NotRun)).then(|| {
                div()
                    .id("ai-probe-result")
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(result)
            });
        let test_header = div()
            .flex()
            .flex_wrap()
            .items_center()
            .justify_between()
            .gap_3()
            .child(
                div()
                    .flex_1()
                    .min_w(px(250.0))
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                Icon::new(gpui_kit_assets::IconName::Sparkles)
                                    .size(px(14.0))
                                    .text_color(rgb(theme.link_foreground)),
                            )
                            .child(div().font_weight(gpui::FontWeight::MEDIUM).child(target)),
                    )
                    .child(
                        div()
                            .text_size(px(10.0))
                            .text_color(rgb(theme.quote_foreground))
                            .child("短いテストメッセージを送信します。文書は送信しません。"),
                    )
                    .child(
                        div()
                            .text_size(px(10.0))
                            .text_color(rgb(theme.quote_foreground))
                            .child("利用料金・利用枠を消費する場合があります。"),
                    ),
            )
            .child(action);
        let test_panel = div()
            .id("ai-probe-test-box")
            .w_full()
            .px(px(14.0))
            .py(px(14.0))
            .rounded(px(8.0))
            .border_1()
            .border_color(rgb(if theme.editor_background < 0x888888 {
                0x373b43
            } else {
                theme.table_border
            }))
            .bg(rgb(if theme.editor_background < 0x888888 {
                0x1e2126
            } else {
                theme.code_background
            }))
            .flex()
            .flex_col()
            .gap_3()
            .child(test_header)
            .children(probe_disabled_reason)
            .children(result_state);
        div()
            .id("ai-probe-section")
            .w_full()
            .text_size(px(12.0))
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .child(
                        div()
                            .text_size(px(16.0))
                            .font_weight(gpui::FontWeight::BOLD)
                            .child("応答を確認"),
                    )
                    .child(
                        div()
                            .text_size(px(10.0))
                            .text_color(rgb(theme.quote_foreground))
                            .child("保存・適用済みの設定が対象です"),
                    ),
            )
            .child(test_panel)
    }

    fn probe_disabled_reason(&self, dirty: bool) -> Option<String> {
        if !T00_PROBE_GATE_PASSED {
            Some("安全プロファイルの確認が終わるまで応答確認は使えません。".to_owned())
        } else if self.service.is_none() {
            Some("AIサービスを利用できません。Markdown編集は引き続き利用できます。".to_owned())
        } else if dirty {
            Some(
                "未保存の変更があります。先に「保存して適用」するか、「変更を破棄」してください。"
                    .to_owned(),
            )
        } else if self.snapshot.ownership != OwnershipState::Owned {
            Some("このHaneがAI runtimeを管理できないため、応答確認は使えません。状態を再確認してください。".to_owned())
        } else if self.snapshot.recovery_required
            || !matches!(
                self.snapshot.persistence,
                PersistenceState::Clean | PersistenceState::Saved
            )
        {
            Some("設定の保存・復旧が必要です。上の案内に従ってください。".to_owned())
        } else if !self.saved_connection_is_configured() {
            Some("先に接続設定とモデルを入力し、「保存して適用」してください。".to_owned())
        } else if self.snapshot.settings.active_connection == ActiveConnection::ChatGpt
            && !matches!(self.snapshot.account, AccountState::SignedIn { .. })
        {
            Some("ChatGPTにログインしてから応答を確認してください。".to_owned())
        } else if self.snapshot.settings.active_connection == ActiveConnection::Custom
            && self
                .snapshot
                .settings
                .custom
                .as_ref()
                .and_then(|custom| custom.credential_ref.as_ref())
                .is_none()
        {
            Some("APIキーの「登録」を押して入力し、「保存して適用」を押してください。".to_owned())
        } else if !self.saved_configuration_is_applied() {
            Some(
                "保存済み設定の適用を確認できません。「適用を再試行」を押してください。".to_owned(),
            )
        } else if self.snapshot.busy.is_some() {
            Some("AIの別操作が進行中です。完了後に再試行してください。".to_owned())
        } else {
            None
        }
    }

    fn save_disabled_reason(&self, dirty: bool, editable: bool, cx: &App) -> &'static str {
        let durability_reconfirmation = self.needs_durability_reconfirmation();
        let endpoint_changed = self.inputs.as_ref().is_some_and(|inputs| {
            let (_, base_url, _) = self.custom_draft_values(inputs, cx);
            endpoint_changed_with_registered_key(
                self.snapshot.settings.custom.as_ref(),
                &base_url,
                self.credential_edit,
            )
        });
        let replace_is_empty = self.credential_edit == CredentialEdit::Replace
            && self
                .inputs
                .as_ref()
                .is_some_and(|inputs| value(&inputs.custom_api_key, cx).is_empty());
        let key_is_missing = self.active_connection == ActiveConnection::Custom
            && self
                .snapshot
                .settings
                .custom
                .as_ref()
                .and_then(|custom| custom.credential_ref.as_ref())
                .is_none()
            && self.credential_edit == CredentialEdit::Keep
            && !durability_reconfirmation;
        if endpoint_changed {
            "接続先変更後は、新しいkeyの登録またはkey削除が必要です。"
        } else if replace_is_empty {
            "新しいAPIキーを入力してから保存してください。"
        } else if key_is_missing {
            "APIキーを登録してから保存してください。"
        } else if !dirty && !durability_reconfirmation {
            "保存する変更はありません。"
        } else if !editable {
            "AI操作中または復旧中のため、いまは設定を変更できません。"
        } else {
            ""
        }
    }

    fn save_section(
        &mut self,
        window: &mut Window,
        cx: &mut Context<EditorView>,
        dirty: bool,
        editable: bool,
        theme: Theme,
    ) -> impl IntoElement {
        let durability_reconfirmation = self.needs_durability_reconfirmation();
        let disabled_reason = self.save_disabled_reason(dirty, editable, cx);
        let view = cx.entity();
        let save_label = if self
            .snapshot
            .busy
            .is_some_and(|(_, reason)| reason == ServiceBusyReason::Saving)
        {
            "保存・適用中…"
        } else if durability_reconfirmation {
            "設定を再保存して復旧"
        } else {
            "保存して適用"
        };
        let save = Button::new("ai-settings-save")
            .label(save_label)
            .primary()
            .disabled(!disabled_reason.is_empty())
            .on_click(move |_, window, app| {
                view.update(app, |view, cx| view.ai_settings.save(window, cx, None))
            });
        let view = cx.entity();
        let discard = Button::new("ai-settings-discard")
            .label("変更を破棄")
            .disabled(!dirty || self.snapshot.busy.is_some())
            .on_click(move |_, window, app| {
                view.update(app, |view, cx| {
                    view.ai_settings.reset_draft(window, cx);
                    cx.notify();
                })
            });
        let _ = window;
        let (state_label, state_note) = if let Some((_, reason)) = self.snapshot.busy {
            match reason {
                ServiceBusyReason::Saving => (
                    "保存・適用中です",
                    "完了後に、適用された接続の状態を更新します。",
                ),
                ServiceBusyReason::Recovering => (
                    "設定を復旧しています",
                    "復旧が終わるまで設定の変更や応答確認はできません。",
                ),
                ServiceBusyReason::Login => (
                    "ChatGPTへログイン中です",
                    "ブラウザーでの認証が終わるまで、この画面を開いたままにできます。",
                ),
                ServiceBusyReason::Account => (
                    "アカウント状態を更新中です",
                    "完了するとChatGPTのログイン状態を表示します。",
                ),
                ServiceBusyReason::Models => (
                    "モデル一覧を取得中です",
                    "取得後に利用するモデルを選べます。",
                ),
                ServiceBusyReason::Probe => (
                    "応答を確認中です",
                    "保存済みの接続へ短いテストメッセージを送信しています。",
                ),
            }
        } else if self.snapshot.persistence == PersistenceState::NotCommitted {
            (
                "設定を保存できませんでした",
                "入力内容を確認して、もう一度「保存して適用」を押してください。",
            )
        } else if dirty {
            (
                "未保存の変更があります",
                if disabled_reason.is_empty() {
                    "保存するまでは、現在有効な接続設定は変わりません。"
                } else {
                    disabled_reason
                },
            )
        } else if self.saved_connection_is_configured() && !self.saved_configuration_is_applied() {
            if self.snapshot.runtime_state == hane_ai::RuntimeState::Failed {
                (
                    "保存した設定を適用できませんでした",
                    "現在利用できる接続先は確認できません。「適用を再試行」から再起動できます。",
                )
            } else {
                (
                    "保存済み・適用状態を確認中です",
                    "保存済みの接続先とモデルは上部に表示しています。",
                )
            }
        } else if self.snapshot.settings.revision == 0 {
            (
                "初回設定が必要です",
                "接続方法と必要な項目を選び、「保存して適用」を押してください。",
            )
        } else if !self.saved_connection_is_configured() {
            (
                "設定が未完成です",
                "必要な項目を入力し、「保存して適用」を押してください。",
            )
        } else {
            ("設定は保存・適用済みです", disabled_reason)
        };
        div()
            .id("ai-settings-savebar")
            .debug_selector(|| "ai-settings-savebar".to_owned())
            .w_full()
            .flex_none()
            .px(px(24.0))
            .py(px(12.0))
            .border_t_1()
            .border_color(rgb(theme.table_border))
            .bg(rgb(theme.sidebar_background))
            .flex()
            .items_center()
            .justify_between()
            .gap_4()
            .child(
                div()
                    .id("ai-settings-save-state")
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(state_label)
                    .child(state_note),
            )
            .child(div().flex().gap_2().child(discard).child(save))
    }

    fn leave_confirmation(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<EditorView>,
        target: LeaveTarget,
    ) -> impl IntoElement {
        let view = cx.entity();
        let save = Button::new("ai-settings-save-and-leave")
            .label("保存して移動")
            .on_click(move |_, window, app| {
                view.update(app, |view, cx| {
                    view.ai_settings.save(window, cx, Some(target))
                })
            });
        let view = cx.entity();
        let discard = Button::new("ai-settings-discard-and-leave")
            .label("破棄して移動")
            .on_click(move |_, window, app| {
                view.update(app, |view, cx| {
                    view.ai_settings.reset_draft(window, cx);
                    view.ai_settings.leave_prompt = None;
                    if target == LeaveTarget::General {
                        view.select_settings_category(
                            super::SettingsCategory::General,
                            window,
                            cx,
                        );
                    } else {
                        view.close_settings(window, cx);
                    }
                })
            });
        let view = cx.entity();
        let stay = Button::new("ai-settings-continue-editing")
            .label("編集を続ける")
            .ghost()
            .on_click(move |_, _, app| {
                view.update(app, |view, cx| {
                    view.ai_settings.leave_prompt = None;
                    cx.notify();
                })
            });
        div()
            .id("ai-settings-leave-confirmation")
            .flex()
            .flex_col()
            .gap_2()
            .child("AI設定に未保存の変更があります。保存して移動するか、変更を破棄してください。")
            .child(div().flex().gap_2().child(save).child(discard).child(stay))
    }
}

impl EditorView {
    pub(super) fn ai_settings_render(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        self.ai_settings.render(window, cx, self.theme)
    }
}

fn value(input: &Entity<InputState>, cx: &App) -> String {
    input.read(cx).value().to_string()
}

fn set_value(
    input: &Entity<InputState>,
    value: &str,
    window: &mut Window,
    cx: &mut Context<EditorView>,
) {
    input.update(cx, |state, cx| state.set_value(value, window, cx));
}

fn editable_field(ownership: OwnershipState, recovery_required: bool) -> bool {
    ownership == OwnershipState::Owned && !recovery_required
}

fn base_url_validation_message(error: hane_ai::CustomProviderConfigError) -> &'static str {
    match error {
        hane_ai::CustomProviderConfigError::NonHttpsBaseUrl => {
            "接続先URLはHTTPSが必要です。HTTPはlocalhost・127.0.0.1・::1での開発に限られます。"
        }
        hane_ai::CustomProviderConfigError::CredentialInUrl => {
            "URLにユーザー名やパスワードを含めないでください。"
        }
        hane_ai::CustomProviderConfigError::InvalidField("base_url") => {
            "接続先URLに改行などの制御文字は使えません。"
        }
        hane_ai::CustomProviderConfigError::InvalidField(_) => "接続先URLを確認してください。",
    }
}

fn model_list_error_message(error: &hane_ai::ModelListError) -> &'static str {
    match error {
        hane_ai::ModelListError::Rpc => "通信エラー",
        hane_ai::ModelListError::InvalidResponse => "応答形式の不一致",
        hane_ai::ModelListError::CursorLoop => "一覧のページ移動エラー",
        hane_ai::ModelListError::TooManyPages | hane_ai::ModelListError::TooManyModels => {
            "一覧が対応上限を超えました"
        }
    }
}

fn probe_error_message(error: ProbeErrorCode) -> &'static str {
    match error {
        ProbeErrorCode::InvalidConfiguration => "設定値を確認してください。",
        ProbeErrorCode::AccountUnavailable => {
            "ChatGPTアカウント状態を取得できません。状態更新または再ログインをお試しください。"
        }
        ProbeErrorCode::ModelUnavailable => "モデルを利用できません。モデルIDを確認してください。",
        ProbeErrorCode::CredentialUnavailable => {
            "APIキーを取得できません。登録状態を確認してください。"
        }
        ProbeErrorCode::OwnedElsewhere => "別のHaneプロセスがAI runtimeを使用しています。",
        ProbeErrorCode::Busy => "別のAI処理が進行中です。完了後に再試行してください。",
        ProbeErrorCode::RuntimeUnavailable => {
            "AI runtimeを利用できません。runtimeを再起動して再試行してください。"
        }
        ProbeErrorCode::StaleGeneration => {
            "設定が更新されています。最新の状態で再試行してください。"
        }
        ProbeErrorCode::Unauthorized | ProbeErrorCode::Forbidden => {
            "認証に失敗しました。ChatGPTは再ログイン、APIキー接続はキーの確認を行ってください。"
        }
        ProbeErrorCode::RateLimited => "利用制限に達しました。時間をおいて再試行してください。",
        ProbeErrorCode::Network => {
            "接続に失敗しました。ネットワークと接続先URLを確認してください。"
        }
        ProbeErrorCode::TimedOut => {
            "時間内に応答しませんでした。設定と接続先を確認して再試行してください。"
        }
        ProbeErrorCode::ProtocolMismatch => "AIサービスの応答形式を確認できませんでした。",
        ProbeErrorCode::NotificationOverflow => {
            "処理結果の通知を確認できません。成功とは扱っていません。"
        }
        ProbeErrorCode::SafetyProfileUnsupported => {
            "安全な応答確認を実行できません。このruntime設定では接続確認を止めています。"
        }
        ProbeErrorCode::ProviderFailed => {
            "Providerが応答できませんでした。別の接続へ自動再送していません。"
        }
        ProbeErrorCode::Canceled => "応答確認を取り消しました。",
        ProbeErrorCode::Isolated => "子プロセスの停止を確認できず、AI操作を停止しています。",
    }
}

fn admission_message(error: AdmissionError) -> String {
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
    }
}

fn current_value_cell(label: &str, value: &str, theme: Theme) -> impl IntoElement {
    let mut value_element = div()
        .text_size(px(15.0))
        .font_weight(gpui::FontWeight::MEDIUM)
        .child(value.to_owned());
    if label.contains("モデル") {
        value_element = value_element
            .font_family("ui-monospace")
            .text_size(px(14.0));
    }
    div()
        .flex_1()
        .min_w(px(130.0))
        .flex()
        .flex_col()
        .gap(px(3.0))
        .child(
            div()
                .text_size(px(10.0))
                .text_color(rgb(theme.quote_foreground))
                .child(label.to_owned()),
        )
        .child(value_element)
}

fn models_hash(value: &str) -> u64 {
    value.bytes().fold(0xcbf29ce484222325, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{Focusable, px};
    use hane_ai::{
        AiService, AiServiceConfig, FakeCredentialStore, ShellEnvironmentPolicyFormat,
        SystemBrowserOpener,
    };
    use hane_document::TextBuffer;
    use std::sync::Arc;
    use std::time::{Instant, SystemTime, UNIX_EPOCH};

    #[test]
    fn connection_and_api_key_choices_explain_when_they_take_effect() {
        assert_eq!(
            connection_name(ActiveConnection::ChatGpt),
            "ChatGPT / Codex"
        );
        assert_eq!(connection_name(ActiveConnection::Custom), "APIキー接続");
        assert_eq!(custom_key_action_label(false), "登録");
        assert_eq!(custom_key_action_label(true), "変更");
        assert!(custom_key_guidance(CredentialEdit::Keep, false).contains("「登録」を押す"));
        assert!(custom_key_guidance(CredentialEdit::Keep, false).contains("接続確認は別操作"));
        assert!(
            custom_key_guidance(CredentialEdit::Replace, true)
                .contains("保存・適用だけでは接続確認")
        );
        assert!(
            custom_key_guidance(CredentialEdit::Delete, true)
                .contains("「保存して適用」を押した後に確定")
        );
    }

    #[gpui::test]
    fn ai_settings_show_only_the_selected_connection_and_keep_savebar_visible(
        cx: &mut gpui::TestAppContext,
    ) {
        eprintln!("stack-budget scenario: create editor");
        let (view, cx) =
            cx.add_window_view(|_, cx| EditorView::new("document body\n", "Untitled", cx));
        cx.simulate_resize(gpui::size(px(1600.0), px(2400.0)));
        view.update(cx, |view, cx| {
            view.settings_open = true;
            view.settings_category = super::SettingsCategory::Connection;
            view.ai_settings.snapshot.ownership = OwnershipState::Owned;
            view.ai_settings.snapshot.persistence = PersistenceState::Clean;
            view.ai_settings.snapshot.settings.custom = Some(CustomConnectionSettings {
                id: "custom".to_owned(),
                name: "Test Provider".to_owned(),
                base_url: "https://provider.example/v1".to_owned(),
                model_id: "model-id".to_owned(),
                credential_ref: Some(hane_ai::CredentialRef::from_persisted("credential-ref")),
            });
            cx.notify();
        });
        eprintln!("stack-budget scenario: render ChatGPT card");
        cx.run_until_parked();
        eprintln!("stack-budget scenario: ChatGPT card rendered");

        assert!(cx.debug_bounds("ai-chatgpt-section").is_some());
        assert!(cx.debug_bounds("ai-custom-section").is_none());
        assert!(cx.debug_bounds("ai-chatgpt-model-lock").is_some());
        assert!(cx.debug_bounds("ai-chatgpt-model-section").is_none());
        assert!(cx.debug_bounds("ai-settings-savebar").is_some());

        let api_key_connection = cx
            .debug_bounds("ai-connection-custom")
            .expect("API key connection selector is shown");
        cx.simulate_click(api_key_connection.center(), gpui::Modifiers::none());
        cx.run_until_parked();
        eprintln!("stack-budget scenario: custom card rendered");

        assert!(cx.debug_bounds("ai-chatgpt-section").is_none());
        assert!(cx.debug_bounds("ai-custom-section").is_some());
        assert!(cx.debug_bounds("ai-settings-savebar").is_some());
        assert!(cx.debug_bounds("ai-custom-key-keep").is_none());
        assert!(cx.debug_bounds("ai-custom-provider-details").is_none());
        let details_button = cx
            .debug_bounds("ai-custom-details-toggle")
            .expect("collapsed connection details are discoverable");
        cx.simulate_click(details_button.center(), gpui::Modifiers::none());
        cx.run_until_parked();
        eprintln!("stack-budget scenario: custom details rendered");
        assert!(cx.debug_bounds("ai-custom-provider-details").is_some());
    }

    #[test]
    fn ai_settings_render_with_windows_stack_budget() {
        // Run in a subprocess because a stack overflow aborts rather than unwinds.
        // libtest creates its test thread using RUST_MIN_STACK; 1 MiB is the
        // Windows executable's default main-thread reserve. Exercise both
        // connection cards and the expanded custom form without a live service.
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .env("RUST_MIN_STACK", "1048576")
            .args([
                "view::ai_settings::tests::ai_settings_show_only_the_selected_connection_and_keep_savebar_visible",
                "--exact",
                "--nocapture",
            ])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "AI settings exceeded the Windows stack budget: {}\n{}\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
    }

    #[gpui::test]
    fn register_api_key_button_reveals_the_masked_input(cx: &mut gpui::TestAppContext) {
        let (view, cx) =
            cx.add_window_view(|_, cx| EditorView::new("document body\n", "Untitled", cx));
        cx.simulate_resize(gpui::size(px(1600.0), px(2400.0)));
        view.update(cx, |view, cx| {
            view.settings_open = true;
            view.settings_category = super::SettingsCategory::Connection;
            view.ai_settings.snapshot.ownership = OwnershipState::Owned;
            view.ai_settings.snapshot.persistence = PersistenceState::Clean;
            view.ai_settings.snapshot.settings.active_connection = ActiveConnection::Custom;
            view.ai_settings.snapshot.settings.custom = Some(CustomConnectionSettings {
                id: "custom".to_owned(),
                name: "Test Provider".to_owned(),
                base_url: "https://provider.example/v1".to_owned(),
                model_id: "model-id".to_owned(),
                credential_ref: None,
            });
            cx.notify();
        });
        cx.run_until_parked();

        assert!(cx.debug_bounds("ai-custom-api-key-input").is_none());
        let register_button = cx
            .debug_bounds("ai-custom-key-replace")
            .expect("API key registration button is rendered");
        cx.simulate_click(register_button.center(), gpui::Modifiers::none());
        cx.run_until_parked();

        assert!(cx.debug_bounds("ai-custom-api-key-input").is_some());
        assert!(view.read_with(cx, |view, _| {
            view.ai_settings.credential_edit == CredentialEdit::Replace
        }));

        let cancel_button = cx
            .debug_bounds("ai-custom-key-cancel")
            .expect("key entry has an explicit cancel action");
        cx.simulate_click(cancel_button.center(), gpui::Modifiers::none());
        cx.run_until_parked();
        assert!(cx.debug_bounds("ai-custom-api-key-input").is_none());
        assert!(view.read_with(cx, |view, _| {
            view.ai_settings.credential_edit == CredentialEdit::Keep
        }));
    }

    #[gpui::test]
    fn fixed_probe_button_submits_a_probe_command(cx: &mut gpui::TestAppContext) {
        let app_data_root = std::env::temp_dir().join(format!(
            "hane-ui-fixed-probe-button-{}-{}",
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
            credential_store: Arc::new(FakeCredentialStore::new()),
            browser_opener: Arc::new(SystemBrowserOpener),
            shell_env_format: ShellEnvironmentPolicyFormat::Filters,
        })
        .unwrap();
        let handle = service.handle();
        let (view, cx) =
            cx.add_window_view(|_, cx| EditorView::new("document body\n", "Untitled", cx));
        cx.simulate_resize(gpui::size(px(1600.0), px(2400.0)));
        view.update(cx, |view, cx| {
            view.settings_open = true;
            view.settings_category = super::SettingsCategory::Connection;
            view.ai_settings.service = Some(handle.clone());
            view.ai_settings.snapshot.ownership = OwnershipState::Owned;
            view.ai_settings.snapshot.persistence = PersistenceState::Clean;
            view.ai_settings.snapshot.settings.revision = 1;
            view.ai_settings.snapshot.settings.settings_generation = 1;
            view.ai_settings.snapshot.settings.active_connection = ActiveConnection::Custom;
            view.ai_settings.snapshot.settings.custom = Some(CustomConnectionSettings {
                id: "custom".to_owned(),
                name: "Test Provider".to_owned(),
                base_url: "https://provider.example/v1".to_owned(),
                model_id: "model-id".to_owned(),
                credential_ref: Some(hane_ai::CredentialRef::from_persisted("credential-ref")),
            });
            view.ai_settings.snapshot.runtime_state = hane_ai::RuntimeState::Ready;
            view.ai_settings.snapshot.runtime_generation = 1;
            view.ai_settings.snapshot.configured_settings_generation = 1;
            cx.notify();
        });
        cx.run_until_parked();

        let button = cx
            .debug_bounds("ai-probe-run")
            .expect("fixed probe button is rendered");
        cx.simulate_click(button.center(), gpui::Modifiers::none());
        cx.run_until_parked();

        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let snapshot = handle.snapshot();
            if snapshot.busy.is_some() || snapshot.last_result.is_some() {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "probe button did not submit a command"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
        let snapshot = handle.snapshot();
        assert!(snapshot.busy.is_some() || snapshot.last_result.is_some());
        assert_ne!(snapshot.probe_status, ProbeStatus::NotRun);

        drop(handle);
        drop(service);
        std::fs::remove_dir_all(app_data_root).unwrap();
    }

    #[gpui::test]
    fn chatgpt_model_selection_can_be_changed_multiple_times_before_saving(
        cx: &mut gpui::TestAppContext,
    ) {
        let (view, cx) =
            cx.add_window_view(|_, cx| EditorView::new("document body\n", "Untitled", cx));
        cx.simulate_resize(gpui::size(px(1600.0), px(2400.0)));
        view.update(cx, |view, cx| {
            view.settings_open = true;
            view.settings_category = super::SettingsCategory::Connection;
            view.ai_settings.snapshot.ownership = OwnershipState::Owned;
            view.ai_settings.snapshot.persistence = PersistenceState::Clean;
            view.ai_settings.snapshot.account = AccountState::SignedIn {
                email: Some("user@example.invalid".to_owned()),
                plan_type: "plus".to_owned(),
            };
            view.ai_settings.snapshot.settings.chatgpt.model_id = Some("saved-model".to_owned());
            view.ai_settings.snapshot.model_list = ModelListState::Loaded(vec![
                hane_ai::ChatGptModel {
                    id: "catalog-id-one".to_owned(),
                    model: "model-one".to_owned(),
                    display_name: "Model One".to_owned(),
                    is_default: false,
                },
                hane_ai::ChatGptModel {
                    id: "catalog-id-two".to_owned(),
                    model: "model-two".to_owned(),
                    display_name: "Model Two".to_owned(),
                    is_default: false,
                },
            ]);
            cx.notify();
        });
        cx.run_until_parked();

        let model_one_selector: &'static str =
            Box::leak(format!("ai-model-{}", models_hash("model-one")).into_boxed_str());
        let picker = cx
            .debug_bounds("ai-chatgpt-model-picker")
            .expect("model selector is rendered");
        cx.simulate_click(picker.center(), gpui::Modifiers::none());
        cx.run_until_parked();
        let model_one = cx
            .debug_bounds(model_one_selector)
            .expect("first model option is rendered in the selector");
        cx.simulate_click(model_one.center(), gpui::Modifiers::none());
        cx.run_until_parked();
        let first_draft = view.read_with(cx, |view, app| {
            value(
                &view
                    .ai_settings
                    .inputs
                    .as_ref()
                    .expect("AI settings inputs are initialized")
                    .chatgpt_model,
                app,
            )
        });
        assert_eq!(first_draft, "model-one");

        let model_two_selector: &'static str =
            Box::leak(format!("ai-model-{}", models_hash("model-two")).into_boxed_str());
        let picker = cx
            .debug_bounds("ai-chatgpt-model-picker")
            .expect("model selector remains available after the draft changes");
        cx.simulate_click(picker.center(), gpui::Modifiers::none());
        cx.run_until_parked();
        let model_two = cx
            .debug_bounds(model_two_selector)
            .expect("second model option remains available after the draft changes");
        cx.simulate_click(model_two.center(), gpui::Modifiers::none());
        cx.run_until_parked();
        let second_draft = view.read_with(cx, |view, app| {
            value(
                &view
                    .ai_settings
                    .inputs
                    .as_ref()
                    .expect("AI settings inputs are initialized")
                    .chatgpt_model,
                app,
            )
        });
        assert_eq!(second_draft, "model-two");
    }

    #[gpui::test]
    fn ai_secret_input_is_masked_and_editor_shortcuts_do_not_reach_document(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(crate::actions::register_key_bindings);
        let (view, cx) =
            cx.add_window_view(|_, cx| EditorView::new("document body\n", "Untitled", cx));
        cx.simulate_resize(gpui::size(px(640.0), px(420.0)));
        view.update(cx, |view, cx| {
            view.editor_mut().insert_text("pending edit").unwrap();
            view.after_input(cx);
            view.editor_mut()
                .set_selection(hane_editor::Selection {
                    anchor: hane_document::SourceOffset(0),
                    active: hane_document::SourceOffset(4),
                })
                .unwrap();
            view.scroll_y = 37.0;
        });
        cx.run_until_parked();
        let editor_before = view.read_with(cx, |view, _| {
            (
                view.editor().document().full_text(),
                view.editor().document().revision(),
                view.editor().selection(),
                view.editor().can_undo(),
                view.sessions.active().is_dirty(),
                view.scroll_y,
            )
        });
        assert!(editor_before.3, "test setup must contain an undoable edit");
        assert!(
            editor_before.4,
            "test setup must contain unsaved document state"
        );

        view.update(cx, |view, cx| {
            view.settings_open = true;
            view.settings_category = super::SettingsCategory::Connection;
            view.ai_settings.snapshot.ownership = OwnershipState::Owned;
            view.ai_settings.snapshot.settings.active_connection = ActiveConnection::Custom;
            view.ai_settings.credential_edit = CredentialEdit::Replace;
            cx.notify();
        });
        cx.run_until_parked();

        let secret_input = view.read_with(cx, |view, _| {
            view.ai_settings
                .inputs
                .as_ref()
                .expect("editable AI settings create inputs")
                .custom_api_key
                .clone()
        });
        cx.update(|window, app| {
            secret_input.update(app, |state, cx| {
                window.focus(&state.focus_handle(cx), cx);
            });
        });
        cx.simulate_input("dummy-secret-canary");
        cx.run_until_parked();

        view.read_with(cx, |_, app| {
            assert_eq!(
                secret_input.read(app).value().as_ref(),
                "dummy-secret-canary"
            );
            assert!(secret_input.read(app).presentation().is_masked());
        });
        cx.update(|window, app| {
            secret_input.update(app, |state, cx| state.select_all(window, cx));
        });
        view.read_with(cx, |_, app| {
            assert!(
                secret_input
                    .read(app)
                    .context_menu_capabilities()
                    .has_selection()
            );
            assert!(
                !secret_input
                    .read(app)
                    .context_menu_capabilities()
                    .is_copyable()
            );
        });

        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        assert!(view.read_with(cx, |view, _| view.settings_open));

        cx.simulate_keystrokes(if cfg!(target_os = "macos") {
            "cmd-s"
        } else {
            "ctrl-s"
        });
        cx.run_until_parked();
        let editor_after = view.read_with(cx, |view, _| {
            (
                view.editor().document().full_text(),
                view.editor().document().revision(),
                view.editor().selection(),
                view.editor().can_undo(),
                view.sessions.active().is_dirty(),
                view.scroll_y,
            )
        });
        assert_eq!(editor_after, editor_before);
        assert!(view.read_with(cx, |view, _| view.settings_open));
    }

    #[gpui::test]
    fn external_open_does_not_close_an_open_settings_page(cx: &mut gpui::TestAppContext) {
        let path = std::env::temp_dir().join(format!(
            "hane-ai-settings-external-open-{}.md",
            std::process::id()
        ));
        std::fs::write(&path, "opened from Explorer\n").unwrap();
        let (view, cx) = cx.add_window_view(|_, cx| EditorView::new("", "Untitled", cx));
        cx.simulate_resize(gpui::size(px(640.0), px(420.0)));
        view.update(cx, |view, cx| {
            view.settings_open = true;
            view.settings_category = super::SettingsCategory::Connection;
            cx.notify();
        });

        view.update(cx, |view, cx| {
            view.open_external_path(&path, cx);
        });
        cx.run_until_parked();

        assert!(view.read_with(cx, |view, _| view.settings_open));
        assert_eq!(
            view.read_with(cx, |view, _| view.editor().document().full_text()),
            "opened from Explorer\n"
        );
        cx.update(|window, app| {
            view.update(app, |view, cx| view.handle_settings_escape(window, cx));
        });
        assert!(!view.read_with(cx, |view, _| view.settings_open));
        assert_eq!(
            view.read_with(cx, |view, _| view.editor().document().full_text()),
            "opened from Explorer\n"
        );
        std::fs::remove_file(path).unwrap();
    }

    #[gpui::test]
    fn escape_prompts_before_discarding_a_dirty_ai_settings_draft(cx: &mut gpui::TestAppContext) {
        let (view, cx) =
            cx.add_window_view(|_, cx| EditorView::new("document body\n", "Untitled", cx));
        cx.simulate_resize(gpui::size(px(640.0), px(420.0)));
        view.update(cx, |view, cx| {
            view.settings_open = true;
            view.settings_category = super::SettingsCategory::Connection;
            view.ai_settings.snapshot.ownership = OwnershipState::Owned;
            view.ai_settings.snapshot.persistence = PersistenceState::Clean;
            cx.notify();
        });
        cx.run_until_parked();
        view.update(cx, |view, cx| {
            view.ai_settings.active_connection = ActiveConnection::Custom;
            cx.notify();
        });
        cx.run_until_parked();
        assert!(view.read_with(cx, |view, app| view.ai_settings.is_dirty(app)));

        cx.update(|window, app| {
            view.update(app, |view, cx| view.handle_settings_escape(window, cx));
        });

        assert!(view.read_with(cx, |view, _| view.settings_open));
        assert_eq!(
            view.read_with(cx, |view, _| view.ai_settings.leave_prompt),
            Some(LeaveTarget::Close)
        );
    }

    #[gpui::test]
    fn durability_unconfirmed_settings_can_be_resaved_without_edits(cx: &mut gpui::TestAppContext) {
        let (view, cx) =
            cx.add_window_view(|_, cx| EditorView::new("document body\n", "Untitled", cx));
        cx.simulate_resize(gpui::size(px(640.0), px(420.0)));
        view.update(cx, |view, cx| {
            view.settings_open = true;
            view.settings_category = super::SettingsCategory::Connection;
            view.ai_settings.snapshot.ownership = OwnershipState::Owned;
            view.ai_settings.snapshot.recovery_required = true;
            view.ai_settings.snapshot.persistence = PersistenceState::DurabilityUnconfirmed;
            cx.notify();
        });
        cx.run_until_parked();

        assert!(view.read_with(cx, |view, app| {
            view.ai_settings.inputs.is_some()
                && !view.ai_settings.is_dirty(app)
                && view.ai_settings.needs_durability_reconfirmation()
                && view.ai_settings.settings_editable()
                && view
                    .ai_settings
                    .save_disabled_reason(
                        view.ai_settings.is_dirty(app),
                        view.ai_settings.settings_editable(),
                        app,
                    )
                    .is_empty()
        }));
        cx.update(|window, app| {
            view.update(app, |view, cx| view.ai_settings.save(window, cx, None));
        });
        cx.run_until_parked();
        assert_eq!(
            view.read_with(cx, |view, _| view.ai_settings.message.clone()),
            Some("AIサービスを利用できません。".to_owned()),
            "the re-save button must be enabled even when the settings are unchanged"
        );
    }

    #[gpui::test]
    fn non_owner_and_cleanup_pending_states_offer_recovery_without_edit_controls(
        cx: &mut gpui::TestAppContext,
    ) {
        let (view, cx) =
            cx.add_window_view(|_, cx| EditorView::new("document body\n", "Untitled", cx));
        cx.simulate_resize(gpui::size(px(640.0), px(420.0)));
        view.update(cx, |view, cx| {
            view.settings_open = true;
            view.settings_category = super::SettingsCategory::Connection;
            view.ai_settings.snapshot.ownership = OwnershipState::OwnedElsewhere;
            cx.notify();
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds("settings-sidebar").is_some());
        assert!(view.read_with(cx, |view, _| view.ai_settings.inputs.is_none()));

        view.update(cx, |view, cx| {
            view.ai_settings.snapshot.ownership = OwnershipState::Owned;
            view.ai_settings.snapshot.recovery_required = true;
            view.ai_settings.snapshot.persistence = PersistenceState::CleanupPending;
            cx.notify();
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds("settings-sidebar").is_some());
        assert!(view.read_with(cx, |view, _| view.ai_settings.inputs.is_none()));
    }
}
