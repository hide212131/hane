use super::EditorView;
use gpui::{
    App, AppContext, Context, Entity, Focusable, InteractiveElement, IntoElement, ParentElement,
    Styled, Window, div, px, rgb,
};
use gpui_component::Disableable;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{Input, InputContentType, InputState};
use gpui_component::{Selectable, Sizable};
use hane_ai::{
    AccountState, ActiveConnection, AdmissionError, AiCommand, AiServiceHandle, AiSettings,
    AiSnapshot, ChatGptConnectionSettings, CustomConnectionSettings, LoginState, ModelListState,
    OperationId, OwnershipState, PersistenceState, ProbeStatus, SafeOperationResult,
    ServiceBusyReason,
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
    credential_edit: CredentialEdit,
    message: Option<String>,
    leave_prompt: Option<LeaveTarget>,
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
            credential_edit: CredentialEdit::Keep,
            message: None,
            leave_prompt: None,
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
                    Ok(snapshot) => {
                        if view
                            .update(cx, |view, cx| {
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
        self.credential_edit = CredentialEdit::Keep;
        self.message = None;
        self.leave_prompt = None;
        self.save_then_leave = None;
        self.route_ready = None;
    }

    pub(super) fn close_settings(&mut self) {
        // Dropping the masked InputState releases the transient secret field.
        // It does not cancel app-owned OAuth or Probe operations.
        self.inputs = None;
        self.credential_edit = CredentialEdit::Keep;
        self.leave_prompt = None;
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
        let custom = saved.custom.as_ref();
        self.active_connection != saved.active_connection
            || value(&inputs.chatgpt_model, cx)
                != saved.chatgpt.model_id.as_deref().unwrap_or_default()
            || value(&inputs.custom_name, cx) != custom.map_or("", |custom| custom.name.as_str())
            || value(&inputs.custom_base_url, cx)
                != custom.map_or("", |custom| custom.base_url.as_str())
            || value(&inputs.custom_model, cx)
                != custom.map_or("", |custom| custom.model_id.as_str())
            || self.credential_edit != CredentialEdit::Keep
            || (self.credential_edit == CredentialEdit::Replace
                && !value(&inputs.custom_api_key, cx).is_empty())
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

    fn ensure_inputs(&mut self, window: &mut Window, cx: &mut Context<EditorView>) {
        if self.inputs.is_some()
            || self.snapshot.ownership != OwnershipState::Owned
            || self.snapshot.recovery_required
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
                    .placeholder("新しいAPI key")
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
        self.credential_edit = CredentialEdit::Keep;
        self.leave_prompt = None;
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
        let name = value(&inputs.custom_name, cx);
        let base_url = value(&inputs.custom_base_url, cx);
        let model_id = value(&inputs.custom_model, cx);
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
        let expected_revision = self.snapshot.settings.revision;
        let old_credential = self
            .snapshot
            .settings
            .custom
            .as_ref()
            .and_then(|custom| custom.credential_ref.clone());
        let result = match self.credential_edit {
            CredentialEdit::Replace => {
                let Some(inputs) = &self.inputs else { return };
                let secret = value(&inputs.custom_api_key, cx);
                if secret.is_empty() {
                    self.message = Some(
                        "新しいAPI keyを入力するか、「変更しない」を選択してください。".to_owned(),
                    );
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
            }
            Err(error) => self.message = Some(admission_message(error)),
        }
    }

    pub(super) fn render(
        &mut self,
        window: &mut Window,
        cx: &mut Context<EditorView>,
    ) -> impl IntoElement {
        self.ensure_inputs(window, cx);
        let editable = self.snapshot.ownership == OwnershipState::Owned
            && !self.snapshot.recovery_required
            && self.snapshot.busy.is_none()
            && matches!(
                self.snapshot.persistence,
                PersistenceState::Clean | PersistenceState::Saved
            );
        let ready = self.snapshot.ownership == OwnershipState::Owned
            && !self.snapshot.recovery_required
            && self.inputs.is_some();
        let dirty = self.is_dirty(cx);
        let view = cx.entity();
        let recover = Button::new("ai-settings-retry-open")
            .label("AI状態を再確認")
            .small()
            .on_click(move |_, _, app| {
                view.update(app, |view, cx| view.ai_settings.activate(cx));
            });
        let status = self.status_element();
        let mut body = div()
            .id("ai-settings-page")
            .w_full()
            .max_w(px(760.0))
            .px(px(32.0))
            .py(px(28.0))
            .flex()
            .flex_col()
            .gap_4()
            .child(
                div()
                    .text_size(px(22.0))
                    .font_weight(gpui::FontWeight::BOLD)
                    .child("AI"),
            )
            .child(status);
        if matches!(
            self.snapshot.ownership,
            OwnershipState::Unknown | OwnershipState::OwnedElsewhere | OwnershipState::Unavailable
        ) || self.snapshot.recovery_required
        {
            body = body.child(recover);
        }
        if ready {
            body = body
                .child(self.connection_section(cx, editable))
                .child(self.chatgpt_section(cx, dirty, window))
                .child(self.custom_section(cx, dirty, editable))
                .child(self.probe_section())
                .child(self.save_section(window, cx, dirty, editable));
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
        let _ = window;
        body
    }

    fn status_element(&self) -> impl IntoElement {
        let ownership = match self.snapshot.ownership {
            OwnershipState::Unknown => "AIサービス状態を確認しています。",
            OwnershipState::Owned => "このHaneがAI runtimeを管理しています。",
            OwnershipState::OwnedElsewhere => "AIは別のHaneプロセスで使用中です。",
            OwnershipState::Unavailable => "AI状態を読み込めません。",
        };
        let runtime = match self.snapshot.runtime_state {
            hane_ai::RuntimeState::Stopped => "停止中",
            hane_ai::RuntimeState::Starting => "起動中",
            hane_ai::RuntimeState::Initializing => "初期化中",
            hane_ai::RuntimeState::Ready => "準備完了",
            hane_ai::RuntimeState::Stopping => "停止中",
            hane_ai::RuntimeState::Failed => "エラー",
        };
        let persistence = match self.snapshot.persistence {
            PersistenceState::Clean => "保存済みの設定です。",
            PersistenceState::NotCommitted => "設定は保存されていません。",
            PersistenceState::Saved => "設定を保存しました。接続確認は実行していません。",
            PersistenceState::CleanupPending => {
                "設定は保存済みです。秘密情報の後処理が残っているため、復旧が必要です。"
            }
            PersistenceState::DurabilityUnconfirmed => {
                "保存処理は反映された可能性がありますが、耐久性を確認できません。"
            }
            PersistenceState::RecoveryRequired => {
                "復旧が必要です。復旧完了まで設定を変更できません。"
            }
        };
        div()
            .id("ai-runtime-status")
            .flex()
            .flex_col()
            .gap_1()
            .child(ownership)
            .child(format!("Runtime: {runtime} / App Serverの同梱版は未確認"))
            .child(persistence)
    }

    fn connection_section(
        &mut self,
        cx: &mut Context<EditorView>,
        editable: bool,
    ) -> impl IntoElement {
        let saved = self.snapshot.settings.active_connection;
        let draft = self.active_connection;
        let view = cx.entity();
        let chatgpt = Button::new("ai-connection-chatgpt")
            .label("ChatGPT / Codex")
            .selected(draft == ActiveConnection::ChatGpt)
            .disabled(!editable)
            .on_click(move |_, _, app| {
                view.update(app, |view, cx| {
                    view.ai_settings.active_connection = ActiveConnection::ChatGpt;
                    cx.notify();
                })
            });
        let view = cx.entity();
        let custom = Button::new("ai-connection-custom")
            .label("Custom Provider")
            .selected(draft == ActiveConnection::Custom)
            .disabled(!editable)
            .on_click(move |_, _, app| {
                view.update(app, |view, cx| {
                    view.ai_settings.active_connection = ActiveConnection::Custom;
                    cx.notify();
                })
            });
        div()
            .id("ai-connection-section")
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .font_weight(gpui::FontWeight::BOLD)
                    .child("使用する接続"),
            )
            .child(if draft == saved {
                "保存済みの接続"
            } else {
                "未保存の接続変更"
            })
            .child(div().flex().gap_2().child(chatgpt).child(custom))
    }

    fn chatgpt_section(
        &self,
        cx: &mut Context<EditorView>,
        dirty: bool,
        window: &Window,
    ) -> gpui::AnyElement {
        let Some(inputs) = &self.inputs else {
            return div().into_any_element();
        };
        let active_saved = self.snapshot.settings.active_connection == ActiveConnection::ChatGpt;
        let actions_allowed = active_saved
            && !dirty
            && self.snapshot.busy.is_none()
            && !self.snapshot.recovery_required;
        let account = match &self.snapshot.account {
            AccountState::Unknown => "未確認".to_owned(),
            AccountState::SignedOut => "ログアウト中".to_owned(),
            AccountState::SignedIn { email, plan_type } => format!(
                "ログイン済み{} ({plan_type})",
                email
                    .as_deref()
                    .map(|email| format!(": {email}"))
                    .unwrap_or_default()
            ),
            AccountState::ApiKey => "API key接続".to_owned(),
        };
        let model_list = match &self.snapshot.model_list {
            ModelListState::NotLoaded => "モデル一覧は未取得です。".to_owned(),
            ModelListState::Loading => "モデル一覧を取得しています…".to_owned(),
            ModelListState::Loaded(models) => format!("{}件のモデルを取得しました。", models.len()),
            ModelListState::Failed(_) => {
                "モデル一覧を取得できませんでした。保存済みモデルは変更していません。".to_owned()
            }
        };
        let view = cx.entity();
        let mut models = div().flex().flex_col().gap_1();
        if let ModelListState::Loaded(available) = &self.snapshot.model_list {
            for model in available.iter().take(40) {
                let value = model.model.clone();
                let selected = value
                    == self
                        .snapshot
                        .settings
                        .chatgpt
                        .model_id
                        .as_deref()
                        .unwrap_or_default();
                let label = format!("{} ({})", model.display_name, model.model);
                let view = view.clone();
                let model_button = Button::new(format!("ai-model-{}", models_hash(&value)))
                    .label(label)
                    .small()
                    .selected(selected)
                    .disabled(!actions_allowed)
                    .on_click(move |_, window, app| {
                        view.update(app, |view, cx| {
                            if let Some(inputs) = &view.ai_settings.inputs {
                                inputs.chatgpt_model.update(cx, |state, cx| {
                                    state.set_value(value.clone(), window, cx)
                                });
                            }
                            cx.notify();
                        })
                    });
                models = models.child(model_button);
            }
        }
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
        let service = self.service.clone();
        let view = cx.entity();
        let logout = Button::new("ai-chatgpt-logout")
            .label("ログアウト")
            .small()
            .disabled(
                !actions_allowed || !matches!(self.snapshot.account, AccountState::SignedIn { .. }),
            )
            .on_click(move |_, _, app| {
                if let Some(service) = &service
                    && let Err(error) = service.try_submit(AiCommand::Logout)
                {
                    view.update(app, |view, cx| {
                        view.ai_settings.message = Some(admission_message(error));
                        cx.notify();
                    });
                }
            });
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
        let _ = window;
        div()
            .id("ai-chatgpt-section")
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .font_weight(gpui::FontWeight::BOLD)
                    .child("ChatGPT / Codex"),
            )
            .child(format!("アカウント: {account}"))
            .child(
                div().flex().gap_2().children(
                    [
                        Some(login),
                        cancel_login,
                        Some(refresh_account),
                        Some(logout),
                    ]
                    .into_iter()
                    .flatten(),
                ),
            )
            .child(model_list)
            .child(
                div().child(
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
            .child(if model_stale {
                "保存済みモデルは一覧で見つかりません。自動変更はしていません。"
            } else {
                "モデル一覧から選択するか、model IDを入力してください。"
            })
            .child(models)
            .child(refresh_models)
            .into_any_element()
    }

    fn custom_section(
        &mut self,
        cx: &mut Context<EditorView>,
        dirty: bool,
        editable: bool,
    ) -> gpui::AnyElement {
        let Some(inputs) = &self.inputs else {
            return div().into_any_element();
        };
        let custom = self.snapshot.settings.custom.as_ref();
        let registered = custom
            .and_then(|custom| custom.credential_ref.as_ref())
            .is_some();
        let disabled = !editable || self.snapshot.busy.is_some();
        let view = cx.entity();
        let keep_key = Button::new("ai-custom-key-keep")
            .label("登録済みkeyを維持")
            .small()
            .selected(self.credential_edit == CredentialEdit::Keep)
            .on_click(move |_, window, app| {
                view.update(app, |view, cx| {
                    view.ai_settings.credential_edit = CredentialEdit::Keep;
                    if let Some(input) = &view.ai_settings.inputs {
                        set_value(&input.custom_api_key, "", window, cx);
                    }
                    cx.notify();
                })
            });
        let view = cx.entity();
        let replace_key = Button::new("ai-custom-key-replace")
            .label("keyを置き換える")
            .small()
            .selected(self.credential_edit == CredentialEdit::Replace)
            .disabled(disabled)
            .on_click(move |_, _, app| {
                view.update(app, |view, cx| {
                    view.ai_settings.credential_edit = CredentialEdit::Replace;
                    cx.notify();
                })
            });
        let view = cx.entity();
        let delete_key = Button::new("ai-custom-key-delete")
            .label("保存済みkeyを削除")
            .small()
            .selected(self.credential_edit == CredentialEdit::Delete)
            .disabled(disabled || !registered)
            .on_click(move |_, window, app| {
                view.update(app, |view, cx| {
                    view.ai_settings.credential_edit = CredentialEdit::Delete;
                    if let Some(input) = &view.ai_settings.inputs {
                        set_value(&input.custom_api_key, "", window, cx);
                    }
                    cx.notify();
                })
            });
        let key_input = if self.credential_edit == CredentialEdit::Replace {
            Some(
                Input::new(&inputs.custom_api_key)
                    .aria_label("Custom Provider API key")
                    .content_type(InputContentType::NewPassword)
                    .disabled(disabled),
            )
        } else {
            None
        };
        div()
            .id("ai-custom-section")
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .font_weight(gpui::FontWeight::BOLD)
                    .child("Custom Provider"),
            )
            .child(
                div().child(
                    Input::new(&inputs.custom_name)
                        .aria_label("Provider name")
                        .disabled(disabled),
                ),
            )
            .child(
                div().child(
                    Input::new(&inputs.custom_base_url)
                        .aria_label("Provider base URL")
                        .disabled(disabled),
                ),
            )
            .child(
                div().child(
                    Input::new(&inputs.custom_model)
                        .aria_label("Custom model ID")
                        .disabled(disabled),
                ),
            )
            .child(if registered {
                "API key: 登録済み（値は表示しません）"
            } else {
                "API key: 未登録"
            })
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(keep_key)
                    .child(replace_key)
                    .child(delete_key),
            )
            .children(key_input)
            .child(
                if self.snapshot.settings.active_connection == ActiveConnection::Custom && dirty {
                    "変更はまだ保存されていません。"
                } else {
                    "Custom設定はChatGPT接続と別に保持されます。"
                },
            )
            .into_any_element()
    }

    fn probe_section(&self) -> impl IntoElement {
        let result = match &self.snapshot.probe_status {
            ProbeStatus::NotRun => "まだ接続確認を実行していません。",
            ProbeStatus::Running => "固定入力で応答を確認しています…",
            ProbeStatus::Succeeded => "最後の固定入力確認は成功しました。",
            ProbeStatus::Canceled => "接続確認を取り消しました。",
            ProbeStatus::TimedOut => "接続確認が時間切れになりました。",
            ProbeStatus::Stale => "前回結果は設定変更により古くなっています。",
            ProbeStatus::Isolated => "子プロセスの停止が未確認のため、AI操作を隔離しています。",
            ProbeStatus::Failed(_) => "接続確認に失敗しました。別Providerへの再送はしていません。",
        };
        div()
            .id("ai-probe-section")
            .flex()
            .flex_col()
            .gap_2()
            .child(div().font_weight(gpui::FontWeight::BOLD).child("接続・応答確認"))
            .child("固定入力のみを送ります。現在の文書や選択範囲は添付しません。利用料金や利用枠を消費する場合があります。")
            .child(result)
            .child(if let Some(result) = &self.snapshot.probe_result {
                div().id("ai-probe-plain-response").child(result.text.clone()).into_any_element()
            } else { div().into_any_element() })
            .child(Button::new("ai-probe-run")
                .label(if T00_PROBE_GATE_PASSED { "固定入力で接続を確認" } else { "接続確認は安全性の確認後に利用できます" })
                .disabled(!T00_PROBE_GATE_PASSED))
    }

    fn save_section(
        &mut self,
        window: &mut Window,
        cx: &mut Context<EditorView>,
        dirty: bool,
        editable: bool,
    ) -> impl IntoElement {
        let view = cx.entity();
        let save = Button::new("ai-settings-save")
            .label("保存")
            .disabled(!dirty || !editable)
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
        div()
            .id("ai-settings-save-section")
            .flex()
            .flex_col()
            .gap_2()
            .child(if dirty {
                "未保存の変更があります。"
            } else {
                "すべての変更は保存済みです。"
            })
            .child(div().flex().gap_2().child(save).child(discard))
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
                        view.select_settings_category(false, window, cx);
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
        self.ai_settings.render(window, cx)
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

fn models_hash(value: &str) -> u64 {
    value.bytes().fold(0xcbf29ce484222325, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{Focusable, px};
    use hane_document::TextBuffer;

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
            view.settings_ai_page = true;
            view.ai_settings.snapshot.ownership = OwnershipState::Owned;
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
            view.settings_ai_page = true;
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
    fn non_owner_and_cleanup_pending_states_offer_recovery_without_edit_controls(
        cx: &mut gpui::TestAppContext,
    ) {
        let (view, cx) =
            cx.add_window_view(|_, cx| EditorView::new("document body\n", "Untitled", cx));
        cx.simulate_resize(gpui::size(px(640.0), px(420.0)));
        view.update(cx, |view, cx| {
            view.settings_open = true;
            view.settings_ai_page = true;
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
