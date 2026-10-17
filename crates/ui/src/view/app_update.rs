//! Application update UI (Issue #510, stage U3).
//!
//! Three separate concerns live here, all deliberately free of network, ZIP or
//! filesystem work on the UI thread:
//!
//! 1. The plain-data contract with the app-owned update service
//!    ([`AppUpdateController`] / [`AppUpdateSnapshot`]) and the settings card
//!    that renders it. Nothing here talks to the network; the card only sends
//!    commands when the user presses a button.
//! 2. [`EditorView::prepare_update_restart`], the short, synchronous restart
//!    preparation check. It inspects every session and settings form, persists
//!    drafts through a `Result`-returning path, and either reports every
//!    blocker or an observation a later stage can re-validate. It never saves,
//!    discards, decides a Save As path, starts a helper or quits.
//! 3. The shared external-open entrance ([`EditorView::submit_external_open`]
//!    and [`ExternalOpenQueue`]) so a request that arrives while the restart
//!    freeze is held is answered `Busy` and stays queued instead of being
//!    consumed.

use super::*;

/// Where the update service currently is, as far as the UI needs to know.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AppUpdatePhase {
    Idle,
    Checking,
    UpToDate,
    Available,
    Downloading,
    Verifying,
    Ready,
    /// The prepared update is being deleted in the background.
    Discarding,
    Failed,
    /// Updates cannot be offered by this build at all (for example, no trusted
    /// update key is embedded). Shown with a diagnostic code and no actions.
    Unavailable,
}

/// A point-in-time copy of the update service state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AppUpdateSnapshot {
    pub current_version: String,
    pub phase: AppUpdatePhase,
    /// Opaque identifier of the candidate shown (`Available`, `Downloading`,
    /// `Verifying`, `Ready`). Echoed back by `download` so a click rendered
    /// for one candidate can never start another.
    pub candidate_id: Option<String>,
    pub candidate_version: Option<String>,
    pub received_bytes: u64,
    pub total_bytes: u64,
    /// Stable machine-readable failure code (`Failed` / `Unavailable`).
    pub error_code: Option<String>,
    /// A local re-validation of a previously prepared update is running.
    pub restoring: bool,
}

impl AppUpdateSnapshot {
    pub fn idle(current_version: impl Into<String>) -> Self {
        Self {
            current_version: current_version.into(),
            phase: AppUpdatePhase::Idle,
            candidate_id: None,
            candidate_version: None,
            received_bytes: 0,
            total_bytes: 0,
            error_code: None,
            restoring: false,
        }
    }

    /// True while background work is running, so the UI keeps refreshing.
    pub fn is_busy(&self) -> bool {
        self.restoring
            || matches!(
                self.phase,
                AppUpdatePhase::Checking
                    | AppUpdatePhase::Downloading
                    | AppUpdatePhase::Verifying
                    | AppUpdatePhase::Discarding
            )
    }
}

/// The app-owned update service as the view sees it. Every method returns
/// immediately; work happens on the service's own background thread, and
/// results are observed by reading `snapshot` again.
pub trait AppUpdateController: Send + Sync {
    fn snapshot(&self) -> AppUpdateSnapshot;
    /// Starts one stable check. Repeated calls while one runs join it.
    fn check(&self);
    /// Starts downloading `candidate_id`. Ignored unless it is the candidate
    /// the service currently offers.
    fn download(&self, candidate_id: &str);
    /// Cancels a running check or download. Never deletes a Ready update.
    fn cancel(&self);
    /// Deletes the prepared update.
    fn discard_ready(&self);
    /// Re-validates a prepared update left by an earlier run, locally only.
    /// Idempotent; never communicates.
    fn restore_local(&self);
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum CardAction {
    Check,
    Download(String),
    Cancel,
    DiscardReady,
}

/// What the settings card shows for one snapshot. Pure data so the state to
/// text mapping is testable without a window.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CardModel {
    pub headline: String,
    pub detail: Option<String>,
    pub actions: Vec<(CardAction, &'static str)>,
}

fn failure_text(code: &str) -> &'static str {
    match code {
        "network_unavailable" | "timeout" | "truncated" => {
            "ネットワークに接続できませんでした。接続を確認してから再試行してください。"
        }
        "rate_limited" => {
            "GitHubの利用制限に達しました。しばらくしてから再試行してください。"
        }
        "not_found" | "metadata_missing" | "asset_missing" => {
            "このリリースには更新情報がありません。手動でダウンロードしてください。"
        }
        "signature_invalid" | "identity_mismatch" => "更新情報の署名または発行元を確認できませんでした。",
        "catalog_expired" | "clock_invalid" => {
            "更新情報の有効期限を確認できませんでした。端末の日時を確認してください。"
        }
        "target_unsupported" | "protocol_unsupported" | "state_incompatible" => {
            "この環境では自動更新を利用できません。手動でダウンロードしてください。"
        }
        "insufficient_space" => "空き容量が不足しています。",
        "not_applicable" => "現在のバージョンより古いリリースのため更新しません。",
        "trust_unconfigured" => "このビルドには更新情報を検証する鍵が組み込まれていません。",
        "cancelled" => "キャンセルしました。",
        _ => "更新を確認できませんでした。",
    }
}

fn format_bytes(bytes: u64) -> String {
    const MIB: f64 = 1024.0 * 1024.0;
    format!("{:.1} MiB", bytes as f64 / MIB)
}

/// Maps a snapshot to the card text and the buttons it offers. There is no
/// restart action yet: applying a prepared update is wired in a later stage,
/// and an unusable "再起動して更新" must not be shown.
pub(crate) fn card_model(snapshot: &AppUpdateSnapshot) -> CardModel {
    let version = snapshot.candidate_version.as_deref().unwrap_or("?");
    let code = snapshot.error_code.as_deref().unwrap_or("unknown");
    let mut model = CardModel {
        headline: format!("現在のバージョン: {}", snapshot.current_version),
        detail: None,
        actions: Vec::new(),
    };
    if snapshot.restoring {
        model.detail = Some("準備済みの更新ファイルを確認しています…".to_owned());
        return model;
    }
    match snapshot.phase {
        AppUpdatePhase::Idle => model.actions.push((CardAction::Check, "最新に更新する")),
        AppUpdatePhase::Checking => {
            model.detail = Some("更新を確認しています…".to_owned());
            model.actions.push((CardAction::Cancel, "キャンセル"));
        }
        AppUpdatePhase::UpToDate => {
            model.detail = Some("最新バージョンです".to_owned());
            model.actions.push((CardAction::Check, "もう一度確認"));
        }
        AppUpdatePhase::Available => {
            model.detail = Some(format!("{version} に更新できます"));
            if let Some(id) = snapshot.candidate_id.clone() {
                model
                    .actions
                    .push((CardAction::Download(id), "更新をダウンロード"));
            }
        }
        AppUpdatePhase::Downloading => {
            model.detail = Some(if snapshot.total_bytes > 0 {
                format!(
                    "{version} をダウンロードしています… {} / {}",
                    format_bytes(snapshot.received_bytes),
                    format_bytes(snapshot.total_bytes)
                )
            } else {
                format!("{version} をダウンロードしています…")
            });
            model.actions.push((CardAction::Cancel, "キャンセル"));
        }
        AppUpdatePhase::Verifying => {
            model.detail = Some("更新ファイルを確認しています…".to_owned());
            model.actions.push((CardAction::Cancel, "キャンセル"));
        }
        AppUpdatePhase::Ready => {
            model.detail = Some(format!(
                "更新の準備ができました ({version})。適用機能はこのバージョンではまだ利用できません。"
            ));
            model
                .actions
                .push((CardAction::DiscardReady, "準備した更新を削除"));
        }
        AppUpdatePhase::Discarding => {
            model.detail = Some("更新を削除しています…".to_owned());
        }
        AppUpdatePhase::Failed => {
            model.detail = Some(format!("{} (診断コード: {code})", failure_text(code)));
            model.actions.push((CardAction::Check, "再試行"));
        }
        AppUpdatePhase::Unavailable => {
            model.detail = Some(format!("更新情報を利用できません (診断コード: {code})"));
        }
    }
    model
}

/// Why the app cannot be restarted for an update right now. Reporting a
/// blocker never changes any document, form or setting.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RestartBlocker {
    /// A restart check is already running.
    AlreadyPreparing,
    /// A named, untitled or draft document has unsaved edits.
    DirtyDocument(String),
    /// An IME composition (editor, sidebar filter or rename) is active.
    ImeComposition,
    /// A save, autosave or rename write is in flight.
    SaveInProgress(String),
    /// A title sync, tab-close confirmation, rename or context menu is open.
    PendingEdit,
    /// A Save As dialog is open.
    DialogOpen,
    /// A file is still being opened.
    OpenInFlight,
    /// An AI operation, login, install or settings save is running.
    AiBusy,
    /// The AI connection form has unsaved changes.
    AiFormUnsaved,
    /// The AI prompts form has unsaved changes.
    PromptsFormUnsaved,
    /// A settings change (such as the Explorer menu) is being applied.
    SettingsBusy,
    /// A draft could not be written; its content is not safely persisted.
    DraftWriteFailed(String),
    /// Settings or recent files could not be written.
    SettingsWriteFailed(String),
}

impl RestartBlocker {
    /// User-facing reason, in the vocabulary of the existing save controls.
    pub fn message(&self) -> String {
        match self {
            Self::AlreadyPreparing => "再起動の準備中です。".to_owned(),
            Self::DirtyDocument(label) => {
                format!("未保存の文書があります ({label})。保存してから再起動してください。")
            }
            Self::ImeComposition => {
                "未確定の文字があります。確定または取り消してから再起動してください。".to_owned()
            }
            Self::SaveInProgress(label) => format!("保存中の文書があります ({label})。"),
            Self::PendingEdit => "確認中の操作があります。完了してから再起動してください。".to_owned(),
            Self::DialogOpen => "保存先の選択が完了していません。".to_owned(),
            Self::OpenInFlight => "ファイルを開いている途中です。".to_owned(),
            Self::AiBusy => "AIの処理が実行中です。完了してから再起動してください。".to_owned(),
            Self::AiFormUnsaved => "AI設定に未保存の変更があります。".to_owned(),
            Self::PromptsFormUnsaved => "AIプロンプトに未保存の変更があります。".to_owned(),
            Self::SettingsBusy => "設定を反映中です。".to_owned(),
            Self::DraftWriteFailed(detail) => {
                format!("下書きを書き込めませんでした: {detail}")
            }
            Self::SettingsWriteFailed(detail) => {
                format!("設定を書き込めませんでした: {detail}")
            }
        }
    }
}

/// Identity of everything the restart check looked at. A later stage compares
/// it against the live view before honoring any one-shot permission; holding
/// it alone authorizes nothing.
#[derive(Clone)]
pub struct RestartObservation {
    sessions: Vec<(SessionId, u64, Revision)>,
    workspace_generation: u64,
    external_open_generation: u64,
}

/// Result of [`EditorView::prepare_update_restart`].
pub enum RestartPreparation {
    /// Nothing blocked the restart at the time of the check. The freeze has
    /// already been released and editing is available again.
    Clear(RestartObservation),
    /// At least one reason to keep the app open. Everything was left as it was.
    Blocked(Vec<RestartBlocker>),
}

/// Whether the view took a path handed to the shared external-open entrance.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExternalOpenOutcome {
    /// The path was handed to the normal open path.
    Accepted,
    /// The view is frozen for a restart check. The caller keeps the request
    /// and retries; it is neither consumed nor reported as opened.
    Busy,
}

/// Requests from other processes or adapters waiting for the view to accept
/// them. A request leaves the queue only when `accept` answers `Accepted`.
#[derive(Debug, Default)]
pub struct ExternalOpenQueue {
    pending: std::collections::VecDeque<PathBuf>,
}

impl ExternalOpenQueue {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, path: PathBuf) {
        self.pending.push_back(path);
    }

    pub fn len(&self) -> usize {
        self.pending.len()
    }

    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }

    /// Offers queued paths in order. Stops at the first `Busy` answer, leaving
    /// that path (and every later one) queued. Returns how many were accepted.
    pub fn drain_into(&mut self, mut accept: impl FnMut(&Path) -> ExternalOpenOutcome) -> usize {
        let mut accepted = 0;
        while let Some(path) = self.pending.front() {
            match accept(path) {
                ExternalOpenOutcome::Accepted => {
                    self.pending.pop_front();
                    accepted += 1;
                }
                ExternalOpenOutcome::Busy => break,
            }
        }
        accepted
    }
}

impl EditorView {
    /// Installs the app-owned update service handle. Attaching sends no
    /// request; the service only communicates after an explicit button press.
    pub fn attach_app_update(&mut self, controller: Option<Arc<dyn AppUpdateController>>) {
        self.app_update = controller;
    }

    /// Re-validates a prepared update from an earlier run (local files only)
    /// and keeps the card refreshing while that runs.
    pub(super) fn restore_app_update_local(&mut self, cx: &mut Context<Self>) {
        let Some(controller) = self.app_update.clone() else {
            return;
        };
        controller.restore_local();
        self.poll_app_update(cx);
    }

    /// Redraws the card while the service reports background work. The loop
    /// ends by itself when the service is idle or the view is gone.
    fn poll_app_update(&mut self, cx: &mut Context<Self>) {
        if self.app_update_polling || self.app_update.is_none() {
            return;
        }
        self.app_update_polling = true;
        cx.spawn(async move |view, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(200))
                    .await;
                let keep_going = view.update(cx, |view, cx| {
                    let busy = view
                        .app_update
                        .as_ref()
                        .is_some_and(|controller| controller.snapshot().is_busy());
                    if !busy {
                        view.app_update_polling = false;
                    }
                    cx.notify();
                    busy
                });
                if !matches!(keep_going, Ok(true)) {
                    break;
                }
            }
        })
        .detach();
    }

    pub(super) fn run_app_update_action(&mut self, action: CardAction, cx: &mut Context<Self>) {
        let Some(controller) = self.app_update.clone() else {
            return;
        };
        match action {
            CardAction::Check => controller.check(),
            CardAction::Download(candidate) => controller.download(&candidate),
            CardAction::Cancel => controller.cancel(),
            CardAction::DiscardReady => controller.discard_ready(),
        }
        self.poll_app_update(cx);
        cx.notify();
    }

    /// The "アプリの更新" card of the general settings page. `None` when no
    /// update service is attached (tests, unsupported builds).
    pub(super) fn app_update_card(&self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        let controller = self.app_update.as_ref()?;
        let model = card_model(&controller.snapshot());
        let buttons: Vec<_> = model
            .actions
            .into_iter()
            .enumerate()
            .map(|(index, (action, label))| {
                let view = cx.entity();
                Button::new(format!("settings-app-update-action-{index}"))
                    .label(label)
                    .ghost()
                    .on_click(move |_, _window, app| {
                        let action = action.clone();
                        view.update(app, |view, cx| view.run_app_update_action(action, cx));
                    })
            })
            .collect();
        let card = div()
            .id("settings-app-update-card")
            .debug_selector(|| "settings-app-update-card".to_owned())
            .w_full()
            .px(px(16.0))
            .py(px(14.0))
            .rounded_sm()
            .border_1()
            .border_color(rgb(self.theme.sidebar_active_background))
            .bg(rgb(self.theme.code_background))
            .flex()
            .flex_col()
            .gap_2()
            .child(div().child(model.headline))
            .children(model.detail.map(|detail| {
                div()
                    .id("settings-app-update-detail")
                    .text_color(rgb(self.theme.quote_foreground))
                    .child(detail)
            }))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .gap_2()
                    .children(buttons),
            );
        Some(
            div()
                .pt(px(16.0))
                .border_t_1()
                .border_color(rgb(self.theme.sidebar_active_background))
                .flex()
                .flex_col()
                .gap_2()
                .child(
                    div()
                        .text_size(px(14.0))
                        .font_weight(gpui::FontWeight::BOLD)
                        .child("アプリの更新"),
                )
                .child(card)
                .into_any_element(),
        )
    }

    /// Starts the restart freeze: external opens are answered `Busy` until
    /// [`EditorView::end_restart_freeze`]. Returns false if already frozen.
    pub fn begin_restart_freeze(&mut self) -> bool {
        if self.restart_freeze {
            return false;
        }
        self.restart_freeze = true;
        true
    }

    pub fn end_restart_freeze(&mut self) {
        self.restart_freeze = false;
    }

    pub fn restart_freeze_active(&self) -> bool {
        self.restart_freeze
    }

    /// The one entrance for paths opened from outside the window (another
    /// process on Windows today; Finder or drag-and-drop adapters later).
    /// While the restart freeze is held it returns `Busy` without touching the
    /// view, so the caller can keep the request and retry. An accepted path is
    /// counted so a restart check can tell an open arrived after it looked.
    pub fn submit_external_open(
        &mut self,
        path: &Path,
        cx: &mut Context<Self>,
    ) -> ExternalOpenOutcome {
        if self.restart_freeze {
            return ExternalOpenOutcome::Busy;
        }
        self.external_open_generation = self.external_open_generation.wrapping_add(1);
        self.open_external_path(path, cx);
        ExternalOpenOutcome::Accepted
    }

    fn observe_for_restart(&self) -> RestartObservation {
        RestartObservation {
            sessions: self
                .sessions
                .sessions()
                .map(|session| (session.id(), session.generation(), session.revision()))
                .collect(),
            workspace_generation: self.work_folder_generation,
            external_open_generation: self.external_open_generation,
        }
    }

    /// True while nothing the observation covered has changed.
    pub fn restart_observation_is_current(&self, observation: &RestartObservation) -> bool {
        let now = self.observe_for_restart();
        now.workspace_generation == observation.workspace_generation
            && now.external_open_generation == observation.external_open_generation
            && now.sessions == observation.sessions
    }

    fn restart_blockers(&self, cx: &mut Context<Self>) -> Vec<RestartBlocker> {
        let mut blockers = Vec::new();
        for session in self.sessions.sessions() {
            if session.is_dirty() {
                blockers.push(RestartBlocker::DirtyDocument(session.label()));
            }
            if session.save_in_flight() {
                blockers.push(RestartBlocker::SaveInProgress(session.label()));
            }
        }
        if self
            .sessions
            .sessions()
            .any(|session| session.editor().ime().is_some())
            || self.sidebar_filter_has_composition()
            || self.inline_rename_has_composition()
        {
            blockers.push(RestartBlocker::ImeComposition);
        }
        if !self.title_sync_in_flight.is_empty()
            || !self.title_sync_scheduled.is_empty()
            || !self.title_sync_deferred.is_empty()
            || !self.title_sync_pending.is_empty()
            || self.inline_rename.is_some()
            || self.tab_close_confirm.is_some()
            || !self.tab_close_after_save.is_empty()
            || self.file_tab_context_menu.is_some()
        {
            blockers.push(RestartBlocker::PendingEdit);
        }
        if self.save_as_dialogs_open > 0 {
            blockers.push(RestartBlocker::DialogOpen);
        }
        if !self.loading_paths.is_empty() {
            blockers.push(RestartBlocker::OpenInFlight);
        }
        if let Some(service) = &self.restart_ai_service {
            let snapshot = service.snapshot();
            let login_running = !matches!(
                snapshot.login,
                hane_ai::LoginState::Idle | hane_ai::LoginState::Finished
            );
            if snapshot.busy.is_some()
                || login_running
                || snapshot.runtime_isolated
                || snapshot.recovery_required
            {
                blockers.push(RestartBlocker::AiBusy);
            }
        }
        if self.ai_settings.is_dirty(cx) {
            blockers.push(RestartBlocker::AiFormUnsaved);
        }
        if self.ai_prompts.is_dirty(cx) {
            blockers.push(RestartBlocker::PromptsFormUnsaved);
        }
        if self.file_context_menu_busy {
            blockers.push(RestartBlocker::SettingsBusy);
        }
        blockers
    }

    /// Short synchronous restart check. It holds the restart freeze only for
    /// the duration of this call, and releases it on every outcome, so editing
    /// and the AI service are exactly as they were afterwards. It never saves,
    /// discards, picks a Save As path, starts a helper or quits; a clear
    /// result is an observation, not a permission.
    pub fn prepare_update_restart(&mut self, cx: &mut Context<Self>) -> RestartPreparation {
        if !self.begin_restart_freeze() {
            return RestartPreparation::Blocked(vec![RestartBlocker::AlreadyPreparing]);
        }
        let outcome = self.prepare_update_restart_frozen(cx);
        self.end_restart_freeze();
        cx.notify();
        outcome
    }

    fn prepare_update_restart_frozen(&mut self, cx: &mut Context<Self>) -> RestartPreparation {
        let blockers = self.restart_blockers(cx);
        if !blockers.is_empty() {
            return RestartPreparation::Blocked(blockers);
        }
        let observed = self.observe_for_restart();
        let mut blockers = Vec::new();
        if let Err(failures) = self.flush_pending_drafts_checked() {
            blockers.push(RestartBlocker::DraftWriteFailed(failures.join("; ")));
        }
        if let Err(error) = self.stores.settings().store(&self.settings) {
            blockers.push(RestartBlocker::SettingsWriteFailed(error.to_string()));
        }
        if let Err(error) = self.stores.recent_files().store(&self.recent) {
            blockers.push(RestartBlocker::SettingsWriteFailed(error.to_string()));
        }
        if !blockers.is_empty() {
            return RestartPreparation::Blocked(blockers);
        }
        if !self.restart_observation_is_current(&observed) {
            // Cannot happen within one UI turn, but a changed view must never
            // be reported clear.
            return RestartPreparation::Blocked(vec![RestartBlocker::PendingEdit]);
        }
        RestartPreparation::Clear(observed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    fn snapshot(phase: AppUpdatePhase) -> AppUpdateSnapshot {
        AppUpdateSnapshot {
            phase,
            candidate_id: Some("cand".to_owned()),
            candidate_version: Some("0.27.0".to_owned()),
            ..AppUpdateSnapshot::idle("0.26.0")
        }
    }

    fn labels(model: &CardModel) -> Vec<&'static str> {
        model.actions.iter().map(|(_, label)| *label).collect()
    }

    #[test]
    fn the_card_never_offers_a_restart_and_only_idle_states_offer_a_check() {
        for phase in [
            AppUpdatePhase::Idle,
            AppUpdatePhase::Checking,
            AppUpdatePhase::UpToDate,
            AppUpdatePhase::Available,
            AppUpdatePhase::Downloading,
            AppUpdatePhase::Verifying,
            AppUpdatePhase::Ready,
            AppUpdatePhase::Discarding,
            AppUpdatePhase::Failed,
            AppUpdatePhase::Unavailable,
        ] {
            let model = card_model(&snapshot(phase));
            assert!(
                !labels(&model).iter().any(|label| label.contains("再起動")),
                "{phase:?} must not offer a restart before the OS apply stage exists"
            );
        }
        assert_eq!(
            labels(&card_model(&snapshot(AppUpdatePhase::Idle))),
            ["最新に更新する"]
        );
        assert_eq!(
            labels(&card_model(&snapshot(AppUpdatePhase::Checking))),
            ["キャンセル"]
        );
    }

    #[test]
    fn only_a_verified_up_to_date_result_says_the_app_is_current() {
        for phase in [
            AppUpdatePhase::Idle,
            AppUpdatePhase::Failed,
            AppUpdatePhase::Unavailable,
            AppUpdatePhase::Available,
        ] {
            let mut input = snapshot(phase);
            input.error_code = Some("rate_limited".to_owned());
            let model = card_model(&input);
            let text = format!("{} {}", model.headline, model.detail.unwrap_or_default());
            assert!(!text.contains("最新バージョンです"), "{phase:?}: {text}");
        }
        let model = card_model(&snapshot(AppUpdatePhase::UpToDate));
        assert_eq!(model.detail.as_deref(), Some("最新バージョンです"));
    }

    #[test]
    fn an_unconfigured_build_shows_the_diagnostic_code_and_no_actions() {
        let mut input = snapshot(AppUpdatePhase::Unavailable);
        input.error_code = Some("trust_unconfigured".to_owned());
        let model = card_model(&input);
        assert!(model.actions.is_empty());
        let detail = model.detail.unwrap();
        assert!(detail.contains("更新情報を利用できません"));
        assert!(detail.contains("trust_unconfigured"));
    }

    #[test]
    fn failures_keep_their_code_and_offer_a_retry() {
        let mut input = snapshot(AppUpdatePhase::Failed);
        input.error_code = Some("signature_invalid".to_owned());
        let model = card_model(&input);
        assert!(model.detail.as_deref().unwrap().contains("signature_invalid"));
        assert_eq!(labels(&model), ["再試行"]);
    }

    #[test]
    fn download_is_bound_to_the_candidate_shown() {
        let model = card_model(&snapshot(AppUpdatePhase::Available));
        assert_eq!(
            model.actions,
            vec![(CardAction::Download("cand".to_owned()), "更新をダウンロード")]
        );
    }

    #[test]
    fn a_discarding_update_is_busy_and_offers_no_action() {
        let input = snapshot(AppUpdatePhase::Discarding);
        assert!(input.is_busy());
        let model = card_model(&input);
        assert_eq!(model.detail.as_deref(), Some("更新を削除しています…"));
        assert!(model.actions.is_empty());
        assert!(!snapshot(AppUpdatePhase::Ready).is_busy());
    }

    #[test]
    fn a_ready_update_can_only_be_discarded_for_now() {
        let model = card_model(&snapshot(AppUpdatePhase::Ready));
        assert_eq!(labels(&model), ["準備した更新を削除"]);
    }

    #[test]
    fn the_external_open_queue_keeps_a_busy_request_and_everything_behind_it() {
        let mut queue = ExternalOpenQueue::new();
        queue.push(PathBuf::from("a.md"));
        queue.push(PathBuf::from("b.md"));
        let mut seen = Vec::new();
        let accepted = queue.drain_into(|path| {
            seen.push(path.to_path_buf());
            ExternalOpenOutcome::Busy
        });
        assert_eq!(accepted, 0);
        assert_eq!(seen, vec![PathBuf::from("a.md")]);
        assert_eq!(queue.len(), 2, "a busy answer must not consume the request");

        let mut opened = Vec::new();
        let accepted = queue.drain_into(|path| {
            opened.push(path.to_path_buf());
            ExternalOpenOutcome::Accepted
        });
        assert_eq!(accepted, 2);
        assert_eq!(opened, vec![PathBuf::from("a.md"), PathBuf::from("b.md")]);
        assert!(queue.is_empty());
    }

    #[derive(Default)]
    struct FakeController {
        snapshot: Mutex<Option<AppUpdateSnapshot>>,
        calls: Mutex<Vec<String>>,
    }

    impl FakeController {
        fn new(snapshot: AppUpdateSnapshot) -> Arc<Self> {
            Arc::new(Self {
                snapshot: Mutex::new(Some(snapshot)),
                calls: Mutex::new(Vec::new()),
            })
        }

        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl AppUpdateController for FakeController {
        fn snapshot(&self) -> AppUpdateSnapshot {
            self.snapshot.lock().unwrap().clone().unwrap()
        }
        fn check(&self) {
            self.calls.lock().unwrap().push("check".to_owned());
        }
        fn download(&self, candidate_id: &str) {
            self.calls
                .lock()
                .unwrap()
                .push(format!("download:{candidate_id}"));
        }
        fn cancel(&self) {
            self.calls.lock().unwrap().push("cancel".to_owned());
        }
        fn discard_ready(&self) {
            self.calls.lock().unwrap().push("discard".to_owned());
        }
        fn restore_local(&self) {
            self.calls.lock().unwrap().push("restore".to_owned());
        }
    }

    fn open_general_settings(
        view: &gpui::Entity<EditorView>,
        cx: &mut gpui::VisualTestContext,
    ) {
        view.update(cx, |view, cx| {
            view.settings_open = true;
            view.settings_category = SettingsCategory::General;
            cx.notify();
        });
        cx.run_until_parked();
    }

    #[gpui::test]
    fn attaching_and_opening_settings_send_no_check_or_download(cx: &mut gpui::TestAppContext) {
        let controller = FakeController::new(AppUpdateSnapshot::idle("0.26.0"));
        let (view, cx) = cx.add_window_view(|_, cx| EditorView::new("body\n", "Untitled", cx));
        let attached: Arc<dyn AppUpdateController> = controller.clone();
        view.update(cx, |view, _| view.attach_app_update(Some(attached)));
        open_general_settings(&view, cx);
        assert!(cx.debug_bounds("settings-app-update-card").is_some());
        assert!(
            controller.calls().iter().all(|call| call == "restore"),
            "only local work may happen without a button press: {:?}",
            controller.calls()
        );
    }

    #[gpui::test]
    fn the_card_button_sends_exactly_one_command(cx: &mut gpui::TestAppContext) {
        let controller = FakeController::new(AppUpdateSnapshot::idle("0.26.0"));
        let (view, cx) = cx.add_window_view(|_, cx| EditorView::new("body\n", "Untitled", cx));
        let attached: Arc<dyn AppUpdateController> = controller.clone();
        view.update(cx, |view, _| view.attach_app_update(Some(attached)));
        open_general_settings(&view, cx);
        view.update(cx, |view, cx| {
            view.run_app_update_action(CardAction::Check, cx);
        });
        assert_eq!(controller.calls(), vec!["check".to_owned()]);
        view.update(cx, |view, cx| {
            view.run_app_update_action(CardAction::Download("cand".to_owned()), cx);
        });
        assert_eq!(
            controller.calls(),
            vec!["check".to_owned(), "download:cand".to_owned()]
        );
    }

    #[gpui::test]
    fn the_card_survives_a_settings_category_round_trip(cx: &mut gpui::TestAppContext) {
        let controller = FakeController::new(snapshot(AppUpdatePhase::Ready));
        let (view, cx) = cx.add_window_view(|_, cx| EditorView::new("body\n", "Untitled", cx));
        let attached: Arc<dyn AppUpdateController> = controller.clone();
        view.update(cx, |view, _| view.attach_app_update(Some(attached)));
        open_general_settings(&view, cx);
        view.update(cx, |view, cx| {
            view.settings_category = SettingsCategory::Prompts;
            cx.notify();
        });
        cx.run_until_parked();
        view.update(cx, |view, cx| {
            view.settings_category = SettingsCategory::General;
            cx.notify();
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds("settings-app-update-card").is_some());
        assert_eq!(controller.snapshot().phase, AppUpdatePhase::Ready);
        assert!(
            controller.calls().is_empty(),
            "switching categories must not cancel or discard anything"
        );
    }

    fn draft_root(label: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQUENCE: AtomicU64 = AtomicU64::new(0);
        std::env::temp_dir().join(format!(
            "hane-app-update-{label}-{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ))
    }

    fn expect_blocked(preparation: RestartPreparation) -> Vec<RestartBlocker> {
        match preparation {
            RestartPreparation::Blocked(blockers) => blockers,
            RestartPreparation::Clear(_) => panic!("the restart check must have been blocked"),
        }
    }

    #[gpui::test]
    fn a_clean_untouched_window_is_clear_and_releases_the_freeze(cx: &mut gpui::TestAppContext) {
        let (view, cx) = cx.add_window_view(|_, cx| EditorView::new("", "Untitled", cx));
        let outcome = view.update(cx, |view, cx| view.prepare_update_restart(cx));
        assert!(matches!(outcome, RestartPreparation::Clear(_)));
        view.read_with(cx, |view, _| assert!(!view.restart_freeze_active()));
    }

    #[gpui::test]
    fn a_dirty_document_blocks_and_keeps_its_content(cx: &mut gpui::TestAppContext) {
        let (view, cx) = cx.add_window_view(|_, cx| EditorView::new("", "Untitled", cx));
        view.update(cx, |view, cx| {
            view.editor_mut().insert_text("unsaved").unwrap();
            view.after_input(cx);
        });
        let outcome = view.update(cx, |view, cx| view.prepare_update_restart(cx));
        let blockers = expect_blocked(outcome);
        assert!(
            blockers
                .iter()
                .any(|blocker| matches!(blocker, RestartBlocker::DirtyDocument(_)))
        );
        view.read_with(cx, |view, _| {
            assert_eq!(view.editor().document().full_text(), "unsaved");
            assert!(!view.restart_freeze_active(), "a blocked check must release the freeze");
        });
    }

    #[gpui::test]
    fn a_dirty_background_session_blocks(cx: &mut gpui::TestAppContext) {
        let root = draft_root("background");
        std::fs::create_dir_all(&root).unwrap();
        let work_folder = OsWorkFolderScanner.scan(&root).unwrap();
        let (view, cx) = cx.add_window_view(|_, cx| EditorView::new("", "Untitled", cx));
        view.update(cx, |view, cx| {
            view.work_folder = Some(work_folder);
            view.editor_mut().insert_text("in the background").unwrap();
            view.after_input(cx);
            view.new_work_folder_note(cx);
            assert!(!view.sessions.active().is_dirty());
        });
        let outcome = view.update(cx, |view, cx| view.prepare_update_restart(cx));
        assert!(
            expect_blocked(outcome)
                .iter()
                .any(|blocker| matches!(blocker, RestartBlocker::DirtyDocument(_)))
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    struct FailingDraftStore;

    impl DraftStore for FailingDraftStore {
        fn write(&self, _: &Path, _: DraftId, _: &str) -> std::io::Result<()> {
            Err(std::io::Error::other("disk full"))
        }
        fn remove(&self, _: &Path, _: DraftId) -> std::io::Result<()> {
            Ok(())
        }
        fn recover(&self, _: &Path) -> std::io::Result<RecoveredDrafts> {
            Ok(RecoveredDrafts::default())
        }
    }

    #[gpui::test]
    fn a_draft_write_failure_blocks_the_restart(cx: &mut gpui::TestAppContext) {
        let root = draft_root("draft-failure");
        std::fs::create_dir_all(&root).unwrap();
        let work_folder = OsWorkFolderScanner.scan(&root).unwrap();
        let (view, cx) = cx.add_window_view(|_, cx| EditorView::new("", "Untitled", cx));
        view.update(cx, |view, cx| {
            view.work_folder = Some(work_folder);
            view.new_work_folder_note(cx);
            view.draft_store = Arc::new(FailingDraftStore);
        });
        let outcome = view.update(cx, |view, cx| view.prepare_update_restart(cx));
        assert!(
            expect_blocked(outcome)
                .iter()
                .any(|blocker| matches!(blocker, RestartBlocker::DraftWriteFailed(_)))
        );
        view.read_with(cx, |view, _| assert!(!view.restart_freeze_active()));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[gpui::test]
    fn an_open_save_as_dialog_blocks(cx: &mut gpui::TestAppContext) {
        let (view, cx) = cx.add_window_view(|_, cx| EditorView::new("", "Untitled", cx));
        view.update(cx, |view, _| view.save_as_dialogs_open = 1);
        let outcome = view.update(cx, |view, cx| view.prepare_update_restart(cx));
        assert_eq!(expect_blocked(outcome), vec![RestartBlocker::DialogOpen]);
    }

    #[gpui::test]
    fn a_pending_open_blocks(cx: &mut gpui::TestAppContext) {
        let (view, cx) = cx.add_window_view(|_, cx| EditorView::new("", "Untitled", cx));
        view.update(cx, |view, _| {
            view.loading_paths.insert(PathBuf::from("pending.md"));
        });
        let outcome = view.update(cx, |view, cx| view.prepare_update_restart(cx));
        assert_eq!(expect_blocked(outcome), vec![RestartBlocker::OpenInFlight]);
    }

    #[gpui::test]
    fn an_external_open_during_the_freeze_is_busy_and_not_consumed(cx: &mut gpui::TestAppContext) {
        let path = std::env::temp_dir().join(format!(
            "hane-app-update-open-{}.md",
            std::process::id()
        ));
        std::fs::write(&path, "from outside\n").unwrap();
        let (view, cx) = cx.add_window_view(|_, cx| EditorView::new("", "Untitled", cx));
        view.update(cx, |view, _| assert!(view.begin_restart_freeze()));

        let mut queue = ExternalOpenQueue::new();
        queue.push(path.clone());
        let accepted = view.update(cx, |view, cx| {
            queue.drain_into(|path| view.submit_external_open(path, cx))
        });
        assert_eq!(accepted, 0);
        assert_eq!(queue.len(), 1, "the busy request must stay queued");
        view.read_with(cx, |view, _| {
            assert_eq!(view.editor().document().full_text(), "");
        });

        view.update(cx, |view, _| view.end_restart_freeze());
        let accepted = view.update(cx, |view, cx| {
            queue.drain_into(|path| view.submit_external_open(path, cx))
        });
        cx.run_until_parked();
        assert_eq!(accepted, 1);
        assert!(queue.is_empty());
        view.read_with(cx, |view, _| {
            assert_eq!(view.editor().document().full_text(), "from outside\n");
        });
        std::fs::remove_file(path).unwrap();
    }

    #[gpui::test]
    fn an_open_accepted_after_the_check_makes_the_observation_stale(
        cx: &mut gpui::TestAppContext,
    ) {
        let path = std::env::temp_dir().join(format!(
            "hane-app-update-stale-{}.md",
            std::process::id()
        ));
        std::fs::write(&path, "late\n").unwrap();
        let (view, cx) = cx.add_window_view(|_, cx| EditorView::new("", "Untitled", cx));
        let observation = match view.update(cx, |view, cx| view.prepare_update_restart(cx)) {
            RestartPreparation::Clear(observation) => observation,
            RestartPreparation::Blocked(_) => panic!("a clean window must be clear"),
        };
        view.read_with(cx, |view, _| {
            assert!(view.restart_observation_is_current(&observation));
        });
        view.update(cx, |view, cx| {
            assert_eq!(
                view.submit_external_open(&path, cx),
                ExternalOpenOutcome::Accepted
            );
        });
        view.read_with(cx, |view, _| {
            assert!(!view.restart_observation_is_current(&observation));
        });
        cx.run_until_parked();
        std::fs::remove_file(path).unwrap();
    }
}
