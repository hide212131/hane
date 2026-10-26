//! "ネットワーク" settings page (Issue #623): the app-wide proxy mode, the
//! manual HTTP proxy endpoint / bypass list and its Basic credentials.
//!
//! Opening, editing and saving never communicate. The page edits a draft
//! (strings) apart from the saved [`NetworkSettings`]; a save validates the
//! draft, then writes in a fixed order — new secret into the OS store, the
//! settings file (atomic), and only then the removal of the old secret — so a
//! failure at any step leaves the previous settings in effect. Secret values
//! live only in the masked inputs and the OS store, never in the settings file,
//! the status text or the logs.

use super::{EditorView, SettingsCategory};
use crate::proxy_secret::{OsProxyCredentialStore, ProxySecretWriter};
use crate::theme::Theme;
use gpui::{
    App, AppContext, Context, Entity, Focusable, InteractiveElement, IntoElement, ParentElement,
    Styled, Window, div, px, rgb,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{Input, InputContentType, InputState};
use gpui_component::{Disableable, Selectable};
use hane_network::{
    BasicCredentials, CredentialRef, ManualProxy, NetworkSettings, ProxyAuth, ProxyMode,
};
use hane_session::{NetworkConfig, Settings, StateStores};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum LeaveTarget {
    Category(SettingsCategory),
    Close,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum AuthChoice {
    None,
    Basic,
}

/// What the form currently says. The user name and password are transient:
/// they are never filled from the OS store, only typed.
#[derive(Clone, PartialEq, Eq)]
pub(super) struct NetworkDraft {
    pub mode: ProxyMode,
    pub url: String,
    pub bypass: String,
    pub auth: AuthChoice,
    pub username: String,
    pub password: String,
}

impl std::fmt::Debug for NetworkDraft {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("NetworkDraft")
            .field("mode", &self.mode)
            .field("url", &self.url)
            .field("bypass", &self.bypass)
            .field("auth", &self.auth)
            .finish_non_exhaustive()
    }
}

impl NetworkDraft {
    pub(super) fn from_settings(settings: &NetworkSettings) -> Self {
        let mut draft = Self {
            mode: settings.mode,
            url: String::new(),
            bypass: String::new(),
            auth: AuthChoice::None,
            username: String::new(),
            password: String::new(),
        };
        if let (ProxyMode::Manual, Some(manual)) = (settings.mode, &settings.manual) {
            draft.url = manual.endpoint().to_string();
            draft.bypass = manual.bypass().to_text();
            draft.auth = match manual.auth() {
                ProxyAuth::None => AuthChoice::None,
                ProxyAuth::Basic(_) => AuthChoice::Basic,
            };
        }
        draft
    }

    /// The part of the form that matters for the chosen mode, so a value typed
    /// into a hidden field does not count as an unsaved change.
    pub(super) fn canonical(&self) -> Self {
        let mut draft = self.clone();
        if draft.mode != ProxyMode::Manual {
            draft.url.clear();
            draft.bypass.clear();
            draft.auth = AuthChoice::None;
        }
        if draft.auth != AuthChoice::Basic {
            draft.username.clear();
            draft.password.clear();
        }
        draft
    }
}

/// Per-field validation messages (Japanese, never echoing the input).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct DraftErrors {
    pub url: Option<String>,
    pub bypass: Option<String>,
    pub credentials: Option<String>,
}

impl DraftErrors {
    fn is_empty(&self) -> bool {
        self.url.is_none() && self.bypass.is_none() && self.credentials.is_none()
    }
}

/// A validated draft, ready to be written.
#[derive(Debug)]
pub(super) struct PreparedSave {
    pub settings: NetworkSettings,
    /// A credential that must reach the OS store before the settings file.
    pub new_credentials: Option<(CredentialRef, BasicCredentials)>,
    /// The previously saved credential that is no longer referenced; removed
    /// only after the new settings were stored.
    pub obsolete_ref: Option<CredentialRef>,
}

fn saved_credential_ref(current: Option<&NetworkSettings>) -> Option<CredentialRef> {
    let settings = current?;
    match (settings.mode, &settings.manual) {
        (ProxyMode::Manual, Some(manual)) => match manual.auth() {
            ProxyAuth::Basic(reference) => Some(reference.clone()),
            ProxyAuth::None => None,
        },
        _ => None,
    }
}

/// Validates `draft` against the shared network contract. `current` is the
/// saved valid configuration (if any); empty credential inputs keep its stored
/// credential. `fresh_ref` makes the id of a newly entered credential.
pub(super) fn prepare(
    draft: &NetworkDraft,
    current: Option<&NetworkSettings>,
    fresh_ref: impl FnOnce() -> CredentialRef,
) -> Result<PreparedSave, DraftErrors> {
    let current_ref = saved_credential_ref(current);
    let (settings, new_credentials) = match draft.mode {
        ProxyMode::Environment => (NetworkSettings::default(), None),
        ProxyMode::Direct => (NetworkSettings::direct(), None),
        ProxyMode::Manual => {
            let mut errors = DraftErrors::default();
            if let Err(error) = ManualProxy::new(&draft.url, "", ProxyAuth::None) {
                errors.url = Some(error.to_string());
            }
            if let Err(error) = ManualProxy::new("http://proxy.invalid", &draft.bypass, ProxyAuth::None)
            {
                errors.bypass = Some(error.to_string());
            }
            let mut new_credentials = None;
            let mut auth = ProxyAuth::None;
            if draft.auth == AuthChoice::Basic {
                let (user, password) = (draft.username.as_str(), draft.password.as_str());
                if user.is_empty() && password.is_empty() {
                    match &current_ref {
                        Some(reference) => auth = ProxyAuth::Basic(reference.clone()),
                        None => {
                            errors.credentials =
                                Some("ユーザー名とパスワードを入力してください".to_owned());
                        }
                    }
                } else if user.is_empty() || password.is_empty() {
                    errors.credentials =
                        Some("ユーザー名とパスワードの両方を入力してください".to_owned());
                } else {
                    let credentials = BasicCredentials::new(user, password);
                    if credentials.check_for_basic().is_err() {
                        errors.credentials = Some(
                            "ユーザー名に「:」は使えません。ユーザー名・パスワードに制御文字や改行は使えません"
                                .to_owned(),
                        );
                    } else {
                        let reference = fresh_ref();
                        auth = ProxyAuth::Basic(reference.clone());
                        new_credentials = Some((reference, credentials));
                    }
                }
            }
            if !errors.is_empty() {
                return Err(errors);
            }
            let manual = ManualProxy::new(&draft.url, &draft.bypass, auth).map_err(|error| {
                DraftErrors {
                    url: Some(error.to_string()),
                    ..DraftErrors::default()
                }
            })?;
            (NetworkSettings::manual(manual), new_credentials)
        }
    };
    let kept_ref = match settings.manual.as_ref().map(ManualProxy::auth) {
        Some(ProxyAuth::Basic(reference)) => Some(reference.clone()),
        _ => None,
    };
    let obsolete_ref = current_ref.filter(|reference| Some(reference) != kept_ref.as_ref());
    Ok(PreparedSave {
        settings,
        new_credentials,
        obsolete_ref,
    })
}

/// A fresh opaque credential id (valid per [`CredentialRef`]).
fn fresh_credential_ref() -> CredentialRef {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos());
    let sequence = COUNTER.fetch_add(1, Ordering::Relaxed);
    CredentialRef::new(format!(
        "proxy-{nanos:x}-{:x}-{sequence:x}",
        std::process::id()
    ))
    .expect("a generated credential id is valid")
}

pub(super) struct SaveReport {
    /// The save succeeded but the old credential could not be removed.
    pub warning: Option<String>,
}

/// The write sequence. Any failure before the settings file is replaced leaves
/// the old settings (and the old credential) in effect.
pub(super) fn run_save(
    prepared: &PreparedSave,
    mut base: Settings,
    stores: &StateStores,
    secrets: &dyn ProxySecretWriter,
) -> Result<SaveReport, String> {
    if let Some((reference, credentials)) = &prepared.new_credentials
        && secrets.put(reference, credentials).is_err()
    {
        let _ = secrets.delete(reference);
        return Err(
            "資格情報をOSの秘密ストアに保存できませんでした。設定は変更していません（以前の設定のままです）"
                .to_owned(),
        );
    }
    base.network = NetworkConfig::Valid(prepared.settings.clone());
    if let Err(error) = stores.settings().store(&base) {
        if let Some((reference, _)) = &prepared.new_credentials {
            let _ = secrets.delete(reference);
        }
        return Err(format!(
            "設定を保存できませんでした: {error}。以前の設定のままです"
        ));
    }
    let warning = prepared
        .obsolete_ref
        .as_ref()
        .and_then(|reference| secrets.delete(reference).err())
        .map(|_| "以前の資格情報をOSの秘密ストアから削除できませんでした".to_owned());
    Ok(SaveReport { warning })
}

struct NetworkInputs {
    url: Entity<InputState>,
    bypass: Entity<InputState>,
    username: Entity<InputState>,
    password: Entity<InputState>,
}

enum PageMessage {
    Saved(String),
    Failed(String),
}

pub(super) struct NetworkSettingsPage {
    secrets: Arc<dyn ProxySecretWriter>,
    saved: NetworkConfig,
    baseline: NetworkDraft,
    mode: ProxyMode,
    auth: AuthChoice,
    inputs: Option<NetworkInputs>,
    errors: DraftErrors,
    message: Option<PageMessage>,
    busy: bool,
    generation: u64,
    leave_prompt: Option<LeaveTarget>,
    route_ready: Option<LeaveTarget>,
    pending_target: Option<LeaveTarget>,
}

impl Default for NetworkSettingsPage {
    fn default() -> Self {
        Self::new(Arc::new(OsProxyCredentialStore))
    }
}

fn value(input: &Entity<InputState>, cx: &App) -> String {
    input.read(cx).value().to_string()
}

impl NetworkSettingsPage {
    pub(super) fn new(secrets: Arc<dyn ProxySecretWriter>) -> Self {
        let saved = NetworkConfig::default();
        let baseline = NetworkDraft::from_settings(&NetworkSettings::default());
        Self {
            secrets,
            saved,
            mode: baseline.mode,
            auth: baseline.auth,
            baseline,
            inputs: None,
            errors: DraftErrors::default(),
            message: None,
            busy: false,
            generation: 0,
            leave_prompt: None,
            route_ready: None,
            pending_target: None,
        }
    }

    /// Fills the form from `config` and drops every typed value, including
    /// the masked password.
    fn reset_form(&mut self, config: &NetworkConfig) {
        self.saved = config.clone();
        self.baseline = NetworkDraft::from_settings(&config.valid().cloned().unwrap_or_default());
        self.mode = self.baseline.mode;
        self.auth = self.baseline.auth;
        self.inputs = None;
        self.errors = DraftErrors::default();
    }

    pub(super) fn begin_settings_session(&mut self, config: &NetworkConfig) {
        self.generation = self.generation.wrapping_add(1);
        self.busy = false;
        self.message = None;
        self.leave_prompt = None;
        self.route_ready = None;
        self.pending_target = None;
        self.reset_form(config);
    }

    pub(super) fn close_settings(&mut self, config: &NetworkConfig) {
        self.generation = self.generation.wrapping_add(1);
        self.busy = false;
        self.message = None;
        self.leave_prompt = None;
        self.route_ready = None;
        self.pending_target = None;
        self.reset_form(config);
    }

    pub(super) fn is_busy(&self) -> bool {
        self.busy
    }

    fn current_draft(&self, cx: &App) -> NetworkDraft {
        let mut draft = self.baseline.clone();
        draft.mode = self.mode;
        draft.auth = self.auth;
        if let Some(inputs) = &self.inputs {
            draft.url = value(&inputs.url, cx);
            draft.bypass = value(&inputs.bypass, cx);
            draft.username = value(&inputs.username, cx);
            draft.password = value(&inputs.password, cx);
        }
        draft
    }

    pub(super) fn is_dirty(&self, cx: &App) -> bool {
        self.current_draft(cx).canonical() != self.baseline.canonical()
    }

    pub(super) fn confirm_leave(&mut self, target: LeaveTarget) {
        self.pending_target = None;
        self.route_ready = None;
        self.leave_prompt = Some(target);
    }

    pub(super) fn cancel_pending_leave(&mut self) {
        self.pending_target = None;
        self.route_ready = None;
    }

    /// The last synchronous boundary before leaving: edits made after the save
    /// completed turn the route back into a leave confirmation.
    pub(super) fn take_ready_route(&mut self, cx: &App) -> Option<LeaveTarget> {
        let target = self.route_ready.take()?;
        if self.is_dirty(cx) {
            self.leave_prompt = Some(target);
            return None;
        }
        Some(target)
    }

    pub(super) fn input_has_focus(&self, window: &Window, cx: &App) -> bool {
        self.inputs.as_ref().is_some_and(|inputs| {
            [&inputs.url, &inputs.bypass, &inputs.username, &inputs.password]
                .into_iter()
                .any(|input| input.read(cx).focus_handle(cx).is_focused(window))
        })
    }

    fn set_mode(&mut self, mode: ProxyMode, cx: &mut Context<EditorView>) {
        if self.busy {
            return;
        }
        self.mode = mode;
        self.errors = DraftErrors::default();
        self.message = None;
        cx.notify();
    }

    fn set_auth(&mut self, auth: AuthChoice, cx: &mut Context<EditorView>) {
        if self.busy {
            return;
        }
        self.auth = auth;
        self.errors.credentials = None;
        self.message = None;
        cx.notify();
    }

    /// Discards the draft (the "元に戻す" button and "破棄して移動").
    fn discard(&mut self) {
        let saved = self.saved.clone();
        self.reset_form(&saved);
        self.message = None;
        self.leave_prompt = None;
        self.cancel_pending_leave();
    }

    /// Validates the draft and, if it is acceptable, marks the page busy and
    /// returns the write job. A rejected draft stores nothing.
    fn begin_save(
        &mut self,
        leave: Option<LeaveTarget>,
        cx: &App,
    ) -> Option<(u64, PreparedSave)> {
        if self.busy {
            return None;
        }
        let draft = self.current_draft(cx);
        match prepare(&draft, self.saved.valid(), fresh_credential_ref) {
            Err(errors) => {
                self.errors = errors;
                self.message = None;
                None
            }
            Ok(prepared) => {
                self.errors = DraftErrors::default();
                self.message = None;
                self.busy = true;
                self.leave_prompt = None;
                self.route_ready = None;
                self.pending_target = leave;
                self.generation = self.generation.wrapping_add(1);
                Some((self.generation, prepared))
            }
        }
    }

    fn saved_ok(&mut self, generation: u64, settings: NetworkSettings, warning: Option<String>) {
        if self.generation != generation {
            return;
        }
        self.busy = false;
        self.reset_form(&NetworkConfig::Valid(settings));
        let mut text = "保存しました。通信は行っていません。新しい設定は、アプリを再起動したあとの接続から使われます。実行中の接続には影響しません。".to_owned();
        if let Some(warning) = warning {
            text.push_str("（注意: ");
            text.push_str(&warning);
            text.push('）');
        }
        self.message = Some(PageMessage::Saved(text));
        if let Some(target) = self.pending_target.take() {
            self.route_ready = Some(target);
        }
    }

    fn save_failed(&mut self, generation: u64, message: String) {
        if self.generation != generation {
            return;
        }
        self.busy = false;
        self.pending_target = None;
        self.message = Some(PageMessage::Failed(message));
    }

    fn ensure_inputs(&mut self, window: &mut Window, cx: &mut Context<EditorView>) {
        if self.inputs.is_some() {
            return;
        }
        let baseline = &self.baseline;
        self.inputs = Some(NetworkInputs {
            url: cx.new(|cx| {
                InputState::new(window, cx)
                    .default_value(baseline.url.as_str())
                    .placeholder("http://proxy.example.com:8080")
            }),
            bypass: cx.new(|cx| {
                InputState::new(window, cx)
                    .default_value(baseline.bypass.as_str())
                    .placeholder("localhost,.example.com,10.0.0.1")
            }),
            username: cx.new(|cx| InputState::new(window, cx).placeholder("ユーザー名")),
            password: cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder("パスワード")
                    .masked(true)
            }),
        });
    }

    fn saved_has_credential(&self) -> bool {
        saved_credential_ref(self.saved.valid()).is_some()
    }

    pub(super) fn render(
        &mut self,
        window: &mut Window,
        cx: &mut Context<EditorView>,
        theme: Theme,
    ) -> impl IntoElement {
        self.ensure_inputs(window, cx);
        let dirty = self.is_dirty(cx);
        let mut body = div()
            .id("network-settings-content")
            .debug_selector(|| "network-settings-content".to_owned())
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
                    .child("ネットワーク"),
            )
            .child(
                div()
                    .text_color(rgb(theme.quote_foreground))
                    .child("アプリ全体で使うHTTPプロキシを設定します。設定の表示・編集・保存では通信しません。"),
            );
        if let Some(reason) = self.saved.invalid_reason() {
            body = body.child(
                div()
                    .id("network-settings-stored-error")
                    .debug_selector(|| "network-settings-stored-error".to_owned())
                    .text_color(rgb(0xb42318))
                    .child(format!(
                        "保存済みのネットワーク設定を読み込めません: {reason}。この状態では接続は失敗します（直接接続には切り替わりません）。下で設定して保存すると置き換えます。"
                    )),
            );
        }
        body = body.child(self.mode_section(cx, theme));
        if self.mode == ProxyMode::Manual {
            body = body.child(self.manual_section(cx, theme));
        }
        match &self.message {
            Some(PageMessage::Saved(text)) => {
                body = body.child(
                    div()
                        .id("network-settings-saved")
                        .debug_selector(|| "network-settings-saved".to_owned())
                        .text_color(rgb(theme.quote_foreground))
                        .child(text.clone()),
                );
            }
            Some(PageMessage::Failed(text)) => {
                body = body.child(
                    div()
                        .id("network-settings-error")
                        .debug_selector(|| "network-settings-error".to_owned())
                        .text_color(rgb(0xb42318))
                        .child(format!("保存に失敗しました: {text}")),
                );
            }
            None => {}
        }
        if let Some(target) = self.leave_prompt {
            body = body.child(self.leave_confirmation(cx, target));
        }
        body.child(self.savebar(cx, theme, dirty))
    }

    #[inline(never)]
    fn mode_section(&self, cx: &mut Context<EditorView>, theme: Theme) -> impl IntoElement {
        let busy = self.busy;
        let buttons = [
            (ProxyMode::Environment, "環境変数を使う", "environment"),
            (ProxyMode::Direct, "直接接続", "direct"),
            (ProxyMode::Manual, "手動HTTPプロキシ", "manual"),
        ]
        .map(|(mode, label, key)| {
            let view = cx.entity();
            Button::new(format!("settings-network-mode-{key}"))
                .label(label)
                .ghost()
                .selected(self.mode == mode)
                .disabled(busy)
                .on_click(move |_, _, app| {
                    view.update(app, |view, cx| view.network_settings.set_mode(mode, cx));
                })
        });
        let help = match self.mode {
            ProxyMode::Environment => {
                "起動時の環境変数（https_proxy / http_proxy / all_proxy / no_proxy）から決めます。設定がなければ直接接続します。Finderから起動したアプリにはシェルのexportが渡らないことがあるため、その場合は手動HTTPプロキシを使ってください。"
            }
            ProxyMode::Direct => "環境変数のプロキシを無視して、常に直接接続します。",
            ProxyMode::Manual => "下で指定したHTTPプロキシを使います。環境変数より優先します。",
        };
        div()
            .id("network-settings-mode-card")
            .debug_selector(|| "network-settings-mode-card".to_owned())
            .w_full()
            .px(px(16.0))
            .py(px(14.0))
            .rounded_sm()
            .border_1()
            .border_color(rgb(theme.sidebar_active_background))
            .bg(rgb(theme.code_background))
            .flex()
            .flex_col()
            .gap_2()
            .child(div().font_weight(gpui::FontWeight::BOLD).child("プロキシ方式"))
            .child(div().flex().flex_row().flex_wrap().gap_2().children(buttons))
            .child(
                div()
                    .text_color(rgb(theme.quote_foreground))
                    .child(help),
            )
    }

    #[inline(never)]
    fn manual_section(&self, cx: &mut Context<EditorView>, theme: Theme) -> impl IntoElement {
        let busy = self.busy;
        let label = |text: &'static str| div().font_weight(gpui::FontWeight::BOLD).child(text);
        let help = |text: &'static str| div().text_color(rgb(theme.quote_foreground)).child(text);
        let error = |id: &'static str, text: &Option<String>| {
            text.as_ref().map(|text| {
                div()
                    .id(id)
                    .debug_selector(|| id.to_owned())
                    .text_color(rgb(0xb42318))
                    .child(text.clone())
            })
        };
        let mut card = div()
            .id("network-settings-manual-card")
            .debug_selector(|| "network-settings-manual-card".to_owned())
            .w_full()
            .px(px(16.0))
            .py(px(14.0))
            .rounded_sm()
            .border_1()
            .border_color(rgb(theme.sidebar_active_background))
            .bg(rgb(theme.code_background))
            .flex()
            .flex_col()
            .gap_2();
        if let Some(inputs) = &self.inputs {
            card = card
                .child(label("プロキシURL"))
                .child(
                    Input::new(&inputs.url)
                        .aria_label("プロキシURL")
                        .disabled(busy),
                )
                .child(help(
                    "http://host:port の形式です。ポートを省略すると80です。IPv6は [::1] のように角括弧で指定します。HTTPS・SOCKSプロキシ、PAC/WPAD、OSのプロキシ設定の自動取得は未対応です。",
                ))
                .children(error("network-settings-url-error", &self.errors.url))
                .child(label("除外先（カンマ区切り）"))
                .child(
                    Input::new(&inputs.bypass)
                        .aria_label("プロキシ除外先")
                        .disabled(busy),
                )
                .child(help(
                    "example.com は自身とそのサブドメインに一致します。* は全宛先を直接接続にします。ワイルドカード・CIDR・ポート付きの指定は使えません。localhost・127.0.0.1・::1は常に直接接続します。",
                ))
                .children(error("network-settings-bypass-error", &self.errors.bypass));
        }
        let auth_buttons = [
            (AuthChoice::None, "認証なし", "none"),
            (AuthChoice::Basic, "Basic認証", "basic"),
        ]
        .map(|(choice, text, key)| {
            let view = cx.entity();
            Button::new(format!("settings-network-auth-{key}"))
                .label(text)
                .ghost()
                .selected(self.auth == choice)
                .disabled(busy)
                .on_click(move |_, _, app| {
                    view.update(app, |view, cx| view.network_settings.set_auth(choice, cx));
                })
        });
        card = card
            .child(label("プロキシ認証"))
            .child(div().flex().flex_row().flex_wrap().gap_2().children(auth_buttons))
            .child(help(
                "Basic認証の通信は、HTTPプロキシまでの区間が暗号化されません。NTLM・Kerberos・Digest認証は未対応です。",
            ));
        if self.auth == AuthChoice::Basic
            && let Some(inputs) = &self.inputs
        {
            card = card
                .child(
                    Input::new(&inputs.username)
                        .aria_label("プロキシのユーザー名")
                        .disabled(busy),
                )
                .child(
                    Input::new(&inputs.password)
                        .aria_label("プロキシのパスワード")
                        .content_type(InputContentType::NewPassword)
                        .disabled(busy),
                )
                .child(help(if self.saved_has_credential() {
                    "保存済みの資格情報があります。変更する場合だけ、ユーザー名とパスワードの両方を入力してください。資格情報はOSの秘密ストアに保存し、画面には表示しません。"
                } else {
                    "資格情報はOSの秘密ストアに保存し、設定ファイルには書きません。保存後は画面に表示しません。"
                }))
                .children(error(
                    "network-settings-credentials-error",
                    &self.errors.credentials,
                ));
        }
        card
    }

    #[inline(never)]
    fn savebar(
        &self,
        cx: &mut Context<EditorView>,
        theme: Theme,
        dirty: bool,
    ) -> impl IntoElement {
        let busy = self.busy;
        let view = cx.entity();
        let save = Button::new("settings-network-save")
            .label(if busy { "保存しています…" } else { "保存" })
            .primary()
            .disabled(busy || !(dirty || self.saved.invalid_reason().is_some()))
            .on_click(move |_, _, app| {
                view.update(app, |view, cx| view.save_network_settings(None, cx));
            });
        let view = cx.entity();
        let revert = Button::new("settings-network-revert")
            .label("元に戻す")
            .disabled(busy || !dirty)
            .on_click(move |_, _, app| {
                view.update(app, |view, cx| {
                    view.network_settings.discard();
                    cx.notify();
                });
            });
        div()
            .id("network-settings-savebar")
            .w_full()
            .flex()
            .items_center()
            .gap_2()
            .pt(px(12.0))
            .border_t_1()
            .border_color(rgb(theme.table_border))
            .child(save)
            .child(revert)
            .child(
                div()
                    .id("network-settings-dirty-state")
                    .debug_selector(|| "network-settings-dirty-state".to_owned())
                    .text_color(rgb(theme.quote_foreground))
                    .child(if dirty {
                        "未保存の変更があります"
                    } else {
                        "保存済みの内容です"
                    }),
            )
    }

    #[inline(never)]
    fn leave_confirmation(
        &self,
        cx: &mut Context<EditorView>,
        target: LeaveTarget,
    ) -> impl IntoElement {
        let view = cx.entity();
        let save = Button::new("settings-network-save-and-leave")
            .label("保存して移動")
            .on_click(move |_, _, app| {
                view.update(app, |view, cx| view.save_network_settings(Some(target), cx));
            });
        let view = cx.entity();
        let discard = Button::new("settings-network-discard-and-leave")
            .label("破棄して移動")
            .on_click(move |_, window, app| {
                view.update(app, |view, cx| {
                    view.network_settings.discard();
                    match target {
                        LeaveTarget::Category(category) => {
                            view.select_settings_category(category, window, cx)
                        }
                        LeaveTarget::Close => view.close_settings(window, cx),
                    }
                })
            });
        let view = cx.entity();
        let stay = Button::new("settings-network-continue-editing")
            .label("編集を続ける")
            .ghost()
            .on_click(move |_, _, app| {
                view.update(app, |view, cx| {
                    view.network_settings.leave_prompt = None;
                    view.network_settings.cancel_pending_leave();
                    view.cancel_pending_settings_drop();
                    cx.notify();
                })
            });
        div()
            .id("network-settings-leave-confirmation")
            .debug_selector(|| "network-settings-leave-confirmation".to_owned())
            .flex()
            .flex_col()
            .gap_2()
            .child("ネットワーク設定に未保存の変更があります。保存して移動するか、変更を破棄してください。")
            .child(div().flex().gap_2().child(save).child(discard).child(stay))
    }
}

impl EditorView {
    pub(super) fn network_settings_render(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        self.network_settings.render(window, cx, self.theme)
    }

    /// Validates and writes the network form. The file and OS-store work runs
    /// off the UI thread; nothing here opens a network connection.
    pub(super) fn save_network_settings(
        &mut self,
        leave: Option<LeaveTarget>,
        cx: &mut Context<Self>,
    ) {
        let Some((generation, prepared)) = self.network_settings.begin_save(leave, cx) else {
            cx.notify();
            return;
        };
        let base = self.settings.clone();
        let stores = self.stores.clone();
        let secrets = self.network_settings.secrets.clone();
        let settings = prepared.settings.clone();
        cx.notify();
        cx.spawn(async move |view, cx| {
            let outcome = cx
                .background_executor()
                .spawn(async move { run_save(&prepared, base, &stores, secrets.as_ref()) })
                .await;
            let _ = view.update(cx, |view, cx| {
                view.finish_network_save(generation, settings, outcome, cx)
            });
        })
        .detach();
    }

    fn finish_network_save(
        &mut self,
        generation: u64,
        settings: NetworkSettings,
        outcome: Result<SaveReport, String>,
        cx: &mut Context<Self>,
    ) {
        match outcome {
            Ok(report) => {
                // The file now holds the new settings whether or not the page
                // is still showing this save.
                self.settings.network = NetworkConfig::Valid(settings.clone());
                self.network_settings
                    .saved_ok(generation, settings, report.warning);
            }
            Err(message) => self.network_settings.save_failed(generation, message),
        }
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hane_session::{MemoryStateStore, SettingsRepository};
    use std::collections::BTreeMap;
    use std::io;
    use std::sync::Mutex;

    #[derive(Default)]
    struct FakeSecrets {
        entries: Mutex<BTreeMap<String, String>>,
        fail_put: bool,
        fail_delete: bool,
    }

    impl ProxySecretWriter for FakeSecrets {
        fn put(
            &self,
            reference: &CredentialRef,
            credentials: &BasicCredentials,
        ) -> Result<(), crate::proxy_secret::SecretStoreError> {
            if self.fail_put {
                return Err(crate::proxy_secret::SecretStoreError);
            }
            self.entries.lock().unwrap().insert(
                reference.as_str().to_owned(),
                format!(
                    "{}:{}",
                    credentials.username().expose(),
                    credentials.password().expose()
                ),
            );
            Ok(())
        }

        fn delete(&self, reference: &CredentialRef) -> Result<(), crate::proxy_secret::SecretStoreError> {
            if self.fail_delete {
                return Err(crate::proxy_secret::SecretStoreError);
            }
            self.entries.lock().unwrap().remove(reference.as_str());
            Ok(())
        }
    }

    struct FailingSettings;

    impl SettingsRepository for FailingSettings {
        fn load(&self) -> Settings {
            Settings::default()
        }

        fn store(&self, _: &Settings) -> io::Result<()> {
            Err(io::Error::other("disk full"))
        }
    }

    fn manual_draft(url: &str, bypass: &str) -> NetworkDraft {
        NetworkDraft {
            mode: ProxyMode::Manual,
            url: url.to_owned(),
            bypass: bypass.to_owned(),
            auth: AuthChoice::None,
            username: String::new(),
            password: String::new(),
        }
    }

    fn basic_draft(user: &str, password: &str) -> NetworkDraft {
        NetworkDraft {
            auth: AuthChoice::Basic,
            username: user.to_owned(),
            password: password.to_owned(),
            ..manual_draft("http://proxy.example:3128", "localhost")
        }
    }

    fn fixed_ref(id: &str) -> impl FnOnce() -> CredentialRef + '_ {
        move || CredentialRef::new(id).unwrap()
    }

    fn saved_basic(id: &str) -> NetworkSettings {
        NetworkSettings::manual(
            ManualProxy::new(
                "http://proxy.example:3128",
                "localhost",
                ProxyAuth::Basic(CredentialRef::new(id).unwrap()),
            )
            .unwrap(),
        )
    }

    fn memory_stores() -> StateStores {
        StateStores::memory()
    }

    #[test]
    fn environment_and_direct_need_no_input() {
        let environment = NetworkDraft::from_settings(&NetworkSettings::default());
        let prepared = prepare(&environment, None, fixed_ref("new")).unwrap();
        assert_eq!(prepared.settings, NetworkSettings::default());
        let direct = NetworkDraft {
            mode: ProxyMode::Direct,
            ..environment
        };
        let prepared = prepare(&direct, None, fixed_ref("new")).unwrap();
        assert_eq!(prepared.settings, NetworkSettings::direct());
        assert!(prepared.new_credentials.is_none());
    }

    #[test]
    fn manual_input_is_validated_per_field() {
        for url in [
            "",
            "proxy.example:3128",
            "https://proxy.example:3128",
            "socks5://proxy.example:1080",
            "http://user:pw@proxy.example:3128",
            "http://proxy.example:3128/path",
            "http://proxy.example:3128?x=1",
            "http://proxy.example:3128#frag",
            "http://:3128",
            "http://proxy.example:0",
            "http://proxy.example:99999",
            "http://proxy.example\r\n:3128",
        ] {
            let errors = prepare(&manual_draft(url, ""), None, fixed_ref("new")).unwrap_err();
            assert!(errors.url.is_some(), "{url:?}");
            assert!(errors.bypass.is_none(), "{url:?}");
            assert!(!errors.url.unwrap().contains("pw"));
        }
        for bypass in ["*.example.com", "10.0.0.0/8", "http://a.com", "a.com:8080", "a b"] {
            let errors = prepare(
                &manual_draft("http://proxy.example:3128", bypass),
                None,
                fixed_ref("new"),
            )
            .unwrap_err();
            assert!(errors.bypass.is_some(), "{bypass:?}");
            assert!(errors.url.is_none(), "{bypass:?}");
        }
    }

    #[test]
    fn unsupported_schemes_are_reported_as_unsupported() {
        let errors =
            prepare(&manual_draft("https://proxy.example:3128", ""), None, fixed_ref("n")).unwrap_err();
        assert!(errors.url.unwrap().contains("未対応"));
    }

    #[test]
    fn a_valid_manual_proxy_accepts_ipv6_default_port_and_bypass() {
        let prepared = prepare(
            &manual_draft("http://[2001:db8::1]", " localhost , .corp.example ,, 10.0.0.1 "),
            None,
            fixed_ref("n"),
        )
        .unwrap();
        let manual = prepared.settings.manual.unwrap();
        assert_eq!(manual.endpoint().port(), 80);
        assert_eq!(manual.auth(), &ProxyAuth::None);
        assert!(!manual.bypass().is_empty());
    }

    #[test]
    fn basic_credentials_need_both_parts_and_a_usable_form() {
        for (user, password) in [("", ""), ("alice", ""), ("", "secret"), ("a:b", "secret"), ("alice", "se\ncret")] {
            let errors = prepare(&basic_draft(user, password), None, fixed_ref("n")).unwrap_err();
            let message = errors.credentials.expect("credential error");
            assert!(!message.contains("secret"), "{user:?}");
            assert!(errors.url.is_none());
        }
    }

    #[test]
    fn new_credentials_get_a_fresh_reference_and_retire_the_old_one() {
        let current = saved_basic("old");
        let prepared =
            prepare(&basic_draft("alice", "pw1"), Some(&current), fixed_ref("new")).unwrap();
        let (reference, credentials) = prepared.new_credentials.as_ref().unwrap();
        assert_eq!(reference.as_str(), "new");
        assert_eq!(credentials.username().expose(), "alice");
        assert_eq!(
            prepared.obsolete_ref.as_ref().map(CredentialRef::as_str),
            Some("old")
        );
        let manual = prepared.settings.manual.as_ref().unwrap();
        assert_eq!(
            manual.auth(),
            &ProxyAuth::Basic(CredentialRef::new("new").unwrap())
        );
        assert!(!format!("{prepared:?}").contains("pw1"));
    }

    #[test]
    fn empty_credential_inputs_keep_the_stored_credential() {
        let current = saved_basic("old");
        let prepared = prepare(&basic_draft("", ""), Some(&current), fixed_ref("new")).unwrap();
        assert!(prepared.new_credentials.is_none());
        assert!(prepared.obsolete_ref.is_none());
        assert_eq!(prepared.settings, current);
    }

    #[test]
    fn leaving_basic_or_manual_retires_the_stored_credential() {
        let current = saved_basic("old");
        let none = NetworkDraft {
            auth: AuthChoice::None,
            ..basic_draft("", "")
        };
        let prepared = prepare(&none, Some(&current), fixed_ref("new")).unwrap();
        assert_eq!(prepared.obsolete_ref.unwrap().as_str(), "old");
        let direct = NetworkDraft {
            mode: ProxyMode::Direct,
            ..none
        };
        let prepared = prepare(&direct, Some(&current), fixed_ref("new")).unwrap();
        assert_eq!(prepared.obsolete_ref.unwrap().as_str(), "old");
    }

    #[test]
    fn hidden_fields_do_not_make_the_form_dirty() {
        let baseline = NetworkDraft::from_settings(&NetworkSettings::default());
        let typed = NetworkDraft {
            url: "http://typed.example".to_owned(),
            username: "u".to_owned(),
            password: "p".to_owned(),
            ..baseline.clone()
        };
        assert_eq!(typed.canonical(), baseline.canonical());
        let changed = NetworkDraft {
            mode: ProxyMode::Direct,
            ..baseline.clone()
        };
        assert_ne!(changed.canonical(), baseline.canonical());
        let credentials = NetworkDraft {
            mode: ProxyMode::Manual,
            auth: AuthChoice::Basic,
            ..manual_draft("http://proxy.example:3128", "")
        };
        let saved = NetworkDraft::from_settings(&saved_basic("old"));
        assert_ne!(
            NetworkDraft {
                password: "x".to_owned(),
                ..credentials
            }
            .canonical(),
            saved.canonical()
        );
    }

    #[test]
    fn the_draft_debug_output_hides_the_password() {
        let text = format!("{:?}", basic_draft("alice-user", "hunter2"));
        assert!(!text.contains("hunter2"));
        assert!(!text.contains("alice-user"));
    }

    #[test]
    fn generated_credential_ids_are_valid_and_distinct() {
        let first = fresh_credential_ref();
        let second = fresh_credential_ref();
        assert_ne!(first, second);
    }

    #[test]
    fn a_successful_save_writes_the_secret_then_settings_then_removes_the_old_one() {
        let secrets = FakeSecrets::default();
        secrets
            .entries
            .lock()
            .unwrap()
            .insert("old".to_owned(), "a:b".to_owned());
        let stores = memory_stores();
        let current = saved_basic("old");
        let prepared =
            prepare(&basic_draft("alice", "pw1"), Some(&current), fixed_ref("new")).unwrap();
        let report = run_save(&prepared, Settings::default(), &stores, &secrets).unwrap();
        assert!(report.warning.is_none());
        let entries = secrets.entries.lock().unwrap();
        assert_eq!(entries.keys().map(String::as_str).collect::<Vec<_>>(), ["new"]);
        assert_eq!(
            stores.settings().load().network,
            NetworkConfig::Valid(prepared.settings.clone())
        );
    }

    #[test]
    fn a_secret_store_failure_keeps_the_old_settings_and_credential() {
        let secrets = FakeSecrets {
            fail_put: true,
            ..FakeSecrets::default()
        };
        secrets
            .entries
            .lock()
            .unwrap()
            .insert("old".to_owned(), "a:b".to_owned());
        let stores = memory_stores();
        let old = saved_basic("old");
        stores
            .settings()
            .store(&Settings {
                network: NetworkConfig::Valid(old.clone()),
                ..Settings::default()
            })
            .unwrap();
        let prepared = prepare(&basic_draft("alice", "pw1"), Some(&old), fixed_ref("new")).unwrap();
        let error = run_save(&prepared, Settings::default(), &stores, &secrets)
            .err()
            .expect("the save fails");
        assert!(!error.contains("pw1"));
        assert_eq!(stores.settings().load().network, NetworkConfig::Valid(old));
        assert_eq!(secrets.entries.lock().unwrap().len(), 1);
        assert!(secrets.entries.lock().unwrap().contains_key("old"));
    }

    #[test]
    fn a_settings_store_failure_removes_the_new_secret_and_keeps_the_old_one() {
        let secrets = FakeSecrets::default();
        secrets
            .entries
            .lock()
            .unwrap()
            .insert("old".to_owned(), "a:b".to_owned());
        let stores = StateStores::new(
            Arc::new(FailingSettings),
            Arc::new(MemoryStateStore::default()),
        );
        let current = saved_basic("old");
        let prepared =
            prepare(&basic_draft("alice", "pw1"), Some(&current), fixed_ref("new")).unwrap();
        let error = run_save(&prepared, Settings::default(), &stores, &secrets)
            .err()
            .expect("the save fails");
        assert!(error.contains("以前の設定のまま"));
        let entries = secrets.entries.lock().unwrap();
        assert_eq!(entries.keys().map(String::as_str).collect::<Vec<_>>(), ["old"]);
    }

    #[test]
    fn failing_to_remove_the_old_credential_is_a_warning_not_a_failure() {
        let secrets = FakeSecrets {
            fail_delete: true,
            ..FakeSecrets::default()
        };
        let stores = memory_stores();
        let current = saved_basic("old");
        let prepared =
            prepare(&basic_draft("alice", "pw1"), Some(&current), fixed_ref("new")).unwrap();
        let report = run_save(&prepared, Settings::default(), &stores, &secrets).unwrap();
        assert!(report.warning.is_some());
        assert_eq!(
            stores.settings().load().network,
            NetworkConfig::Valid(prepared.settings)
        );
    }

    #[test]
    fn saving_keeps_the_other_settings() {
        let stores = memory_stores();
        let base = Settings {
            autosave: false,
            default_folder: Some("/notes".into()),
            ..Settings::default()
        };
        let prepared = prepare(&manual_draft("http://proxy.example:3128", ""), None, fixed_ref("n")).unwrap();
        run_save(&prepared, base, &stores, &FakeSecrets::default()).unwrap();
        let loaded = stores.settings().load();
        assert!(!loaded.autosave);
        assert_eq!(loaded.default_folder, Some("/notes".into()));
        assert_eq!(loaded.network.valid().unwrap().mode, ProxyMode::Manual);
    }

    #[test]
    fn the_page_tracks_saved_state_and_ignores_stale_results() {
        let mut page = NetworkSettingsPage::new(Arc::new(FakeSecrets::default()));
        page.begin_settings_session(&NetworkConfig::default());
        let settings = NetworkSettings::direct();

        // A result for an older generation changes nothing.
        page.busy = true;
        let stale = page.generation.wrapping_sub(1);
        page.saved_ok(stale, settings.clone(), None);
        assert!(page.busy);
        assert_eq!(page.baseline.mode, ProxyMode::Environment);

        page.pending_target = Some(LeaveTarget::Close);
        let generation = page.generation;
        page.saved_ok(generation, settings, None);
        assert!(!page.busy);
        assert_eq!(page.baseline.mode, ProxyMode::Direct);
        assert_eq!(page.mode, ProxyMode::Direct);
        assert_eq!(page.route_ready, Some(LeaveTarget::Close));
        assert!(matches!(page.message, Some(PageMessage::Saved(_))));

        page.busy = true;
        page.pending_target = Some(LeaveTarget::Close);
        let generation = page.generation;
        page.save_failed(generation, "x".to_owned());
        assert!(!page.busy);
        assert!(page.pending_target.is_none());
        assert!(matches!(page.message, Some(PageMessage::Failed(_))));
        // A failed save leaves the saved configuration untouched.
        assert_eq!(page.saved.valid().unwrap().mode, ProxyMode::Direct);
    }

    #[test]
    fn a_new_page_shows_environment_without_a_stored_credential() {
        let mut page = NetworkSettingsPage::new(Arc::new(FakeSecrets::default()));
        page.begin_settings_session(&NetworkConfig::default());
        assert!(page.saved.invalid_reason().is_none());
        assert_eq!(page.mode, ProxyMode::Environment);
        assert!(!page.saved_has_credential());
        assert!(!page.is_busy());
    }
}
