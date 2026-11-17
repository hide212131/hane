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
    /// Where the failure happened (`check`, `download`, `write_download`,
    /// `verify`, `save_history`, `publish`, `start`, `placement`, `config`).
    /// The same code can mean different things at different stages.
    pub error_stage: Option<String>,
    /// The service holds local diagnostics that can be copied from the card.
    pub diagnostics_available: bool,
    /// A local re-validation of a previously prepared update is running.
    pub restoring: bool,
    /// The service can really apply a Ready update in this build: the trust
    /// policy is configured and the install is at a supported placement. Only
    /// then is "再起動して更新" offered; `false` keeps the card fail-closed.
    pub restart_supported: bool,
    /// The Ready candidate the restart prompt may be shown for right now: the
    /// automatic path prepared it, it was re-validated, and "後で" has not
    /// suppressed it. `None` means no prompt, whatever the phase is.
    pub prompt_candidate_id: Option<String>,
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
            error_stage: None,
            diagnostics_available: false,
            restoring: false,
            restart_supported: false,
            prompt_candidate_id: None,
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
    /// The local diagnostics text for the settings card (copy). It reads local
    /// records only: no request, no change. `None` when there is none.
    fn diagnostics_text(&self) -> Option<String> {
        None
    }
    /// Adds one failure of another operation (for example the AI response
    /// check) to the same bounded local diagnostics. `stage`, `code` and
    /// `detail` are already limited to safe values by the caller; the service
    /// still redacts and caps them. A save failure never changes the caller.
    fn record_diagnostic(&self, stage: &str, code: &str, detail: &str) {
        let _ = (stage, code, detail);
    }
    /// Re-verifies the Ready candidate `candidate_id` and the install, starts
    /// the update helper and returns once it answered `Prepared`. It may take
    /// a while and is therefore called off the UI thread. Nothing is stopped,
    /// replaced or quit: the application stays open and unchanged until the
    /// caller authorizes the exit. The default refuses.
    fn prepare_restart(&self, candidate_id: &str) -> Result<Box<dyn PreparedUpdate>, String> {
        let _ = candidate_id;
        Err("この環境では更新を適用できません".to_owned())
    }
    /// Applies the already stored "auto update check" setting to the
    /// background checks: `false` cancels only the automatic work. The caller
    /// saves the setting first and calls this only after that succeeded.
    fn set_auto_check(&self, enabled: bool) {
        let _ = enabled;
    }
    /// The restart prompt band for `candidate_id` was actually drawn. Only now
    /// may the service count it as shown and save the first-display
    /// suppression; `snapshot` alone never does. Idempotent.
    fn prompt_shown(&self, candidate_id: &str) {
        let _ = candidate_id;
    }
    /// "後で": hides the restart prompt for `candidate_id` and suppresses it
    /// again for the same version for a while. The Ready update is kept.
    fn snooze_prompt(&self, candidate_id: &str) {
        let _ = candidate_id;
    }
}

/// The saved files of the window, as the restart ticket will carry them. Paths
/// and bounded positions only; never document text.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RestartTicketSnapshot {
    pub work_folder: Option<PathBuf>,
    pub files: Vec<RestartTicketFile>,
    /// Index into `files` of the active document.
    pub active: Option<usize>,
}

/// Which Explorer registrations were enabled before the update, and so which
/// ones the repair guidance may mention.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ExplorerRepairScope {
    pub file: bool,
    pub folder: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RestartTicketFile {
    pub path: PathBuf,
    pub scroll_line: u32,
    pub caret_offset: u32,
}

/// A helper that has answered `Prepared`. Dropping it without
/// [`PreparedUpdate::authorize_exit`] must abort the helper and leave the
/// install untouched.
pub trait PreparedUpdate: Send {
    /// Writes the restart ticket. `permit_is_fresh` is asked last, right
    /// before the write, and a stale permit must leave no ticket behind.
    fn issue_ticket(
        &mut self,
        ticket: &RestartTicketSnapshot,
        permit_is_fresh: &dyn Fn() -> bool,
    ) -> Result<(), String>;
    /// Lets the helper proceed once this process has exited.
    fn authorize_exit(self: Box<Self>) -> Result<(), String>;
    /// Gives the helper up and removes the ticket.
    fn abort(self: Box<Self>);
}

/// Explicit stop and restart of the app-owned AI service around a restart.
pub trait AiLifecycle: Send + Sync {
    /// Stops the service and waits (up to `timeout`) until it is gone.
    fn stop(&self, timeout: Duration) -> Result<(), String>;
    /// Starts the service again after an abandoned restart. This is the one
    /// explicit request: a stop still in flight keeps the start pending.
    fn resume(&self) -> AiResume;
    /// Asks how a resume that is waiting on a stop went. It never starts a
    /// service, so repeating it cannot repeat a start.
    fn poll_resume(&self) -> AiResume;
}

/// How a resume request went.
pub enum AiResume<H = hane_ai::AiServiceHandle> {
    /// The service is running; attach this handle.
    Started(H),
    /// The old service is still stopping and the start is pending.
    Stopping,
    /// The new service could not be started. A later explicit resume may retry.
    Failed,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum CardAction {
    Check,
    Download(String),
    Cancel,
    Restart,
    /// "保存して再起動" for the candidate the card was drawn for.
    SaveAndRestart(String),
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
        "proxy_configuration" => {
            "プロキシの設定または資格情報を利用できませんでした。ネットワーク設定を確認してください。"
        }
        "proxy_authentication" => {
            "プロキシに認証を拒否されました。プロキシの資格情報を確認してください。"
        }
        "proxy_connect" => {
            "プロキシ経由で接続できませんでした。プロキシの設定と接続先の許可を確認してください。"
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

/// A failure text that fits the stage where the failure happened. The code
/// alone (for example `io`) cannot tell a failed check from a failed write
/// after the download, so the stage decides when it is known. `None` falls
/// back to the code-based text.
fn stage_failure_text(stage: &str, code: &str) -> Option<&'static str> {
    match (stage, code) {
        ("start", _) => Some(
            "更新の処理を開始できませんでした。Haneを再起動してから、もう一度お試しください。",
        ),
        ("write_download", _) => Some(
            "更新ファイルを保存する場所を準備できませんでした。空き容量と書き込み権限を確認してください。",
        ),
        ("download", "io" | "insufficient_space") => Some(
            "ダウンロードした更新ファイルを保存できませんでした。空き容量と書き込み権限を確認してください。",
        ),
        ("download", "hash_mismatch" | "size_mismatch") => Some(
            "ダウンロードした更新ファイルの内容が更新情報と一致しませんでした。もう一度取得するか、手動でダウンロードしてください。",
        ),
        ("download", "limit_exceeded") => Some(
            "更新ファイルが想定されたサイズの上限を超えたため、ダウンロードを中止しました。手動でダウンロードしてください。",
        ),
        ("download", "redirect_rejected") => Some(
            "更新ファイルの取得先が許可されていない場所へ転送されたため、ダウンロードを中止しました。手動でダウンロードしてください。",
        ),
        ("verify", "signature_invalid" | "identity_mismatch") => None,
        ("verify", _) => Some(
            "ダウンロードした更新ファイルを検証または展開できませんでした。もう一度取得するか、手動でダウンロードしてください。",
        ),
        ("save_history", _) => Some(
            "更新ファイルは検証できましたが、更新の履歴を保存できませんでした。更新は準備されていません。書き込み権限を確認してください。",
        ),
        ("publish", _) => Some(
            "検証済みの更新を保存先へ移動できませんでした。更新は準備されていません。書き込み権限を確認してください。",
        ),
        _ => None,
    }
}

/// Why updates are unavailable, with what the user can actually do. Install
/// placement failures are named here instead of surfacing later as a generic
/// I/O error after the archive was fetched. Moving the app or reopening it
/// from Finder is not promised to clear a quarantined launch.
fn unavailable_text(code: &str) -> Option<&'static str> {
    match code {
        "app_translocated" => Some(
            "macOSの隔離(App Translocation)された読み取り専用の場所から起動しているため、更新できません。Haneを終了し、配置済みの Hane.app を直接起動し直してください。それでも解消しない場合は、リリースページから手動で更新してください。",
        ),
        "unsupported_install" => Some(
            "このインストール場所のHaneは自動更新に対応していません。リリースページから手動で更新してください。",
        ),
        "install_modified" => Some(
            "インストール済みのアプリの内容を確認できませんでした。Haneを入れ直してください。",
        ),
        "identity_mismatch" => Some(
            "このHaneの所有者が現在の実行ユーザーと異なるため、更新できません。現在のユーザーが所有し書き込める場所にHaneを入れ直してください。それまではリリースページから手動で更新できます。",
        ),
        "io" | "receipt_unwritable" => Some(
            "インストール場所を確認できないため、更新できません。アプリの場所と書き込み権限を確認してください。",
        ),
        _ => None,
    }
}

fn format_bytes(bytes: u64) -> String {
    const MIB: f64 = 1024.0 * 1024.0;
    format!("{:.1} MiB", bytes as f64 / MIB)
}

/// Maps a snapshot to the card text and the buttons it offers. "再起動して更新"
/// is offered only for a Ready update of a service that reports
/// `restart_supported`; an action that cannot work is never shown.
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
            if snapshot.restart_supported {
                model.detail = Some(format!("更新の準備ができました ({version})。"));
                model
                    .actions
                    .push((CardAction::Restart, "再起動して更新"));
            } else {
                model.detail = Some(format!(
                    "更新の準備ができました ({version})。この環境では適用できません。"
                ));
            }
            model
                .actions
                .push((CardAction::DiscardReady, "準備した更新を削除"));
        }
        AppUpdatePhase::Discarding => {
            model.detail = Some("更新を削除しています…".to_owned());
        }
        AppUpdatePhase::Failed => {
            let stage = snapshot.error_stage.as_deref();
            let text = stage
                .and_then(|stage| stage_failure_text(stage, code))
                .unwrap_or_else(|| failure_text(code));
            model.detail = Some(match stage {
                Some(stage) => format!("{text} (診断コード: {code}, 段階: {stage})"),
                None => format!("{text} (診断コード: {code})"),
            });
            model.actions.push((CardAction::Check, "再試行"));
        }
        AppUpdatePhase::Unavailable => {
            model.detail = Some(match unavailable_text(code) {
                Some(text) => format!("{text} (診断コード: {code})"),
                None => format!("更新情報を利用できません (診断コード: {code})"),
            });
        }
    }
    model
}

/// What the restart prompt band shows for one snapshot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PromptModel {
    pub candidate_id: String,
    pub message: String,
}

/// The prompt exists only for a Ready update that can really be applied, is not
/// being restored, and that the service currently allows to be shown. A
/// snapshot without a candidate id or version never prompts.
pub(crate) fn prompt_model(snapshot: &AppUpdateSnapshot) -> Option<PromptModel> {
    if snapshot.phase != AppUpdatePhase::Ready
        || !snapshot.restart_supported
        || snapshot.restoring
    {
        return None;
    }
    let candidate_id = snapshot.candidate_id.as_ref()?;
    let version = snapshot.candidate_version.as_ref()?;
    if snapshot.prompt_candidate_id.as_ref() != Some(candidate_id) {
        return None;
    }
    Some(PromptModel {
        candidate_id: candidate_id.clone(),
        message: format!("新しいバージョン {version} があります。再起動して更新しますか？"),
    })
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
    /// The network (proxy) form has unsaved changes.
    NetworkFormUnsaved,
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
            Self::NetworkFormUnsaved => "ネットワーク設定に未保存の変更があります。".to_owned(),
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
    /// Each path with the unsaved / saving state seen when it was received.
    pending: std::collections::VecDeque<(PathBuf, bool)>,
    /// Set by [`Self::seal_if_empty`] right before the helper is allowed to
    /// replace this process. A sealed queue refuses `try_push`, so the sender
    /// learns that this process can no longer open the request.
    sealed: bool,
}

impl ExternalOpenQueue {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, path: PathBuf) {
        self.pending.push_back((path, false));
    }

    /// Queues `path` unless the queue is sealed; a sealed queue hands the path
    /// back so the sender can deliver it to the next process.
    pub fn try_push(&mut self, path: PathBuf) -> Result<(), PathBuf> {
        self.try_push_protected(path, false)
    }

    /// Like [`Self::try_push`], recording `protected` (the unsaved / saving
    /// state at receipt) with the request.
    pub fn try_push_protected(&mut self, path: PathBuf, protected: bool) -> Result<(), PathBuf> {
        if self.sealed {
            return Err(path);
        }
        self.pending.push_back((path, protected));
        Ok(())
    }

    /// Atomically closes the queue if nothing is waiting. Every request is
    /// either already queued (this returns `false` and the restart is given
    /// up) or refused by `try_push` afterwards; none falls between the two.
    pub fn seal_if_empty(&mut self) -> bool {
        if !self.pending.is_empty() {
            return false;
        }
        self.sealed = true;
        true
    }

    /// Reopens the queue after the restart was given up.
    pub fn unseal(&mut self) {
        self.sealed = false;
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
        self.drain_requests_into(|path, _| accept(path))
    }

    /// [`Self::drain_into`] that also passes the state recorded at receipt.
    pub fn drain_requests_into(
        &mut self,
        mut accept: impl FnMut(&Path, bool) -> ExternalOpenOutcome,
    ) -> usize {
        let mut accepted = 0;
        while let Some((path, protected)) = self.pending.front() {
            match accept(path, *protected) {
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

/// An [`ExternalOpenQueue`] shared between the platform callback that receives
/// documents (Finder) and the window. The window also reads it during the final
/// restart coordination, so a document that arrived after the restart ticket
/// was written cannot be dropped with this process: the restart is given up
/// while any request is still waiting here.
#[derive(Clone, Debug, Default)]
pub struct SharedExternalOpenQueue {
    inner: Arc<std::sync::Mutex<ExternalOpenQueue>>,
    guard: ExternalOpenGuard,
}

/// The window's "any session is dirty or saving" state, shared with whatever
/// receives external requests (Finder callback, Windows pipe). The receiver
/// reads it when a request arrives, so the protection that existed then is not
/// lost while the request waits for the UI to process it.
#[derive(Clone, Debug, Default)]
pub struct ExternalOpenGuard {
    unsaved: Arc<std::sync::atomic::AtomicBool>,
}

impl ExternalOpenGuard {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_unsaved(&self, unsaved: bool) {
        self.unsaved.store(unsaved, std::sync::atomic::Ordering::SeqCst);
    }

    /// Publishes protection *before* a session turns dirty or starts a save.
    /// The sequentially consistent store happens before the state change on
    /// the UI thread, so a receiver either read before the transition (the
    /// request really arrived while clean) or sees `true`; it can never read a
    /// stale `false` after the state has changed. The following
    /// [`EditorView::sync_external_open_guard`] recomputes the real value from
    /// every session, so other sessions' state is never cleared by it.
    pub fn raise(&self) {
        self.set_unsaved(true);
    }

    pub fn unsaved(&self) -> bool {
        self.unsaved.load(std::sync::atomic::Ordering::SeqCst)
    }
}

impl SharedExternalOpenQueue {
    pub fn new() -> Self {
        Self::default()
    }

    /// The guard requests are stamped with when they are pushed.
    pub fn guard(&self) -> ExternalOpenGuard {
        self.guard.clone()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, ExternalOpenQueue> {
        self.inner.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub fn push(&self, path: PathBuf) {
        let protected = self.guard.unsaved();
        self.lock().pending.push_back((path, protected));
    }

    /// See [`ExternalOpenQueue::try_push`]; the request is stamped with the
    /// guard's state at this moment.
    pub fn try_push(&self, path: PathBuf) -> Result<(), PathBuf> {
        let protected = self.guard.unsaved();
        self.lock().try_push_protected(path, protected)
    }

    /// See [`ExternalOpenQueue::seal_if_empty`].
    pub fn seal_if_empty(&self) -> bool {
        self.lock().seal_if_empty()
    }

    pub fn unseal(&self) {
        self.lock().unseal();
    }

    pub fn len(&self) -> usize {
        self.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.lock().is_empty()
    }

    /// Like [`ExternalOpenQueue::drain_into`], but the lock is not held while
    /// `accept` runs (it may look at this queue again). A request leaves the
    /// queue only after it was accepted.
    pub fn drain_into(&self, mut accept: impl FnMut(&Path) -> ExternalOpenOutcome) -> usize {
        self.drain_requests_into(|path, _| accept(path))
    }

    /// [`Self::drain_into`] that also passes the state recorded at receipt.
    pub fn drain_requests_into(
        &self,
        mut accept: impl FnMut(&Path, bool) -> ExternalOpenOutcome,
    ) -> usize {
        let mut accepted = 0;
        loop {
            let Some((path, protected)) = self.lock().pending.front().cloned() else {
                break;
            };
            match accept(&path, protected) {
                ExternalOpenOutcome::Accepted => {
                    self.lock().pending.pop_front();
                    accepted += 1;
                }
                ExternalOpenOutcome::Busy => break,
            }
        }
        accepted
    }
}

impl EditorView {
    /// Installs the queue the platform fills with documents from outside
    /// (Finder). The final restart check treats a waiting request as a reason
    /// to keep this process running.
    pub fn attach_external_open_inbox(&mut self, inbox: Option<SharedExternalOpenQueue>) {
        if let Some(inbox) = &inbox {
            self.external_open_guard = inbox.guard();
        }
        self.external_open_inbox = inbox;
    }

    /// Installs the guard a request receiver reads when a request arrives
    /// (the Windows pipe; the Finder queue brings its own).
    pub fn attach_external_open_guard(&mut self, guard: ExternalOpenGuard) {
        self.external_open_guard = guard;
    }

    /// Raises the guard to true before a session becomes dirty or a save
    /// starts. The next `sync_external_open_guard` recomputes the actual state.
    pub(super) fn raise_external_open_guard(&self) {
        self.external_open_guard.raise();
    }

    /// Publishes whether any session is dirty or saving. Called every render,
    /// so a receiver reading it sees the state as of the latest frame.
    pub(super) fn sync_external_open_guard(&self) {
        self.external_open_guard.set_unsaved(self.has_unsaved_sessions());
    }

    /// Closes the external-open entrance if nothing is waiting. Without an
    /// inbox there is nothing to close.
    fn seal_external_open(&self) -> bool {
        self.external_open_inbox
            .as_ref()
            .is_none_or(|inbox| inbox.seal_if_empty())
    }

    fn unseal_external_open(&self) {
        if let Some(inbox) = &self.external_open_inbox {
            inbox.unseal();
        }
    }

    fn external_open_waiting(&self) -> bool {
        self.external_open_inbox
            .as_ref()
            .is_some_and(|inbox| !inbox.is_empty())
    }

    /// Installs the app-owned update service handle. Attaching sends no
    /// request; the service only communicates after an explicit button press.
    pub fn attach_app_update(&mut self, controller: Option<Arc<dyn AppUpdateController>>) {
        self.app_update = controller;
    }

    /// Starts the window's observer of the update service: it re-validates a
    /// prepared update locally once (no request) and then redraws the view when
    /// the service state changes, so the restart prompt appears without the
    /// settings screen being open. Called once per window.
    pub fn start_app_update_observer(&mut self, cx: &mut Context<Self>) {
        let Some(controller) = self.app_update.clone() else {
            return;
        };
        controller.restore_local();
        cx.spawn(async move |view, cx| {
            let mut last: Option<AppUpdateSnapshot> = None;
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                let alive = view.update(cx, |view, cx| {
                    let Some(controller) = view.app_update.as_ref() else {
                        return false;
                    };
                    let current = controller.snapshot();
                    if last.as_ref() != Some(&current) {
                        last = Some(current);
                        cx.notify();
                    }
                    true
                });
                if !matches!(alive, Ok(true)) {
                    break;
                }
            }
        })
        .detach();
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
        // A new action ends the one-time "updated" notice.
        self.update_completed = None;
        self.explorer_migration_failed = None;
        // A stale notice (e.g. the diagnostics copy result) must not outlive
        // the state it was shown under.
        self.restart_message = None;
        match action {
            CardAction::Check => controller.check(),
            CardAction::Download(candidate) => controller.download(&candidate),
            CardAction::Cancel => controller.cancel(),
            CardAction::Restart => {
                self.start_update_restart(None, cx);
                return;
            }
            CardAction::SaveAndRestart(candidate) => {
                self.start_save_and_restart(&candidate, cx);
                return;
            }
            CardAction::DiscardReady => controller.discard_ready(),
        }
        self.poll_app_update(cx);
        cx.notify();
    }

    /// Hands a newly failed AI response check to the local diagnostics, once.
    pub(super) fn record_ai_probe_diagnostic(&mut self) {
        let Some(diagnostic) = self.ai_settings.take_unrecorded_probe_diagnostic() else {
            return;
        };
        if let Some(controller) = self.app_update.as_ref() {
            controller.record_diagnostic(
                &format!("ai_probe/{}", diagnostic.stage),
                diagnostic.code,
                &diagnostic.cause,
            );
        }
    }

    /// Copies the update diagnostics to the clipboard. The text is already
    /// redacted by the service; this reads small local records only and makes
    /// no request, so it is safe on the UI thread and while updates are
    /// unavailable.
    pub(super) fn copy_app_update_diagnostics(&mut self, cx: &mut Context<Self>) {
        let Some(controller) = self.app_update.clone() else {
            return;
        };
        match controller.diagnostics_text() {
            Some(text) => {
                cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
                self.restart_message = Some("更新の診断情報をコピーしました。".to_owned());
            }
            None => {
                self.restart_message = Some("コピーできる診断情報がありません。".to_owned());
            }
        }
        cx.notify();
    }

    /// Applies and stores the "自動で更新を確認する" setting. The background
    /// checks are told only after the save succeeded; a failed save leaves the
    /// previous value in place and says so on the card.
    pub(super) fn set_auto_update_check(&mut self, enabled: bool, cx: &mut Context<Self>) {
        if self.settings.auto_update_check == enabled {
            return;
        }
        let previous = self.settings.auto_update_check;
        self.settings.auto_update_check = enabled;
        match self.stores.settings().store(&self.settings) {
            Ok(()) => {
                self.settings_store_error = None;
                self.restart_message = None;
                // The service forgets what it counted as shown; the view must
                // report the next display again so its suppression is saved.
                self.app_update_prompt_reported.borrow_mut().take();
                if let Some(controller) = self.app_update.as_ref() {
                    controller.set_auto_check(enabled);
                }
            }
            Err(error) => {
                self.settings.auto_update_check = previous;
                self.restart_message = Some(format!(
                    "自動で更新を確認する設定を保存できませんでした: {error}"
                ));
            }
        }
        cx.notify();
    }

    /// "後で": hides the prompt and keeps the Ready update.
    fn dismiss_update_prompt(&mut self, candidate: &str, cx: &mut Context<Self>) {
        self.restart_message = None;
        if let Some(controller) = self.app_update.as_ref() {
            controller.snooze_prompt(candidate);
        }
        cx.notify();
    }

    /// One button of the restart prompt. It is a tab stop with its own focus
    /// handle, and Enter / Space run the same action as a click.
    fn update_prompt_button(
        cx: &mut Context<Self>,
        border: gpui::Rgba,
        id: &'static str,
        label: &'static str,
        focus: &gpui::FocusHandle,
        activate: impl Fn(&mut Self, &mut Context<Self>) + Clone + 'static,
    ) -> gpui::Stateful<gpui::Div> {
        let on_key = activate.clone();
        div()
            .id(id)
            .debug_selector(|| id.to_owned())
            .track_focus(focus)
            .tab_stop(true)
            .cursor_pointer()
            .px(px(10.0))
            .py(px(4.0))
            .rounded_sm()
            .border_1()
            .border_color(border)
            .child(label)
            .on_key_down(cx.listener(move |view, event: &gpui::KeyDownEvent, _window, cx| {
                if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                    cx.stop_propagation();
                    on_key(view, cx);
                }
            }))
            .on_click(cx.listener(move |view, _, _, cx| activate(view, cx)))
    }

    /// The restart prompt band drawn over the top of the document area. It is
    /// an overlay, so no document coordinate changes. Its buttons are tab stops
    /// but are never focused automatically, so showing the prompt does not move
    /// the editor focus. Hidden
    /// while the settings screen is open (the card has the same actions) and
    /// while a tab close confirmation is shown; nothing is recorded as shown then.
    pub(super) fn update_prompt_element(&self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        if self.settings_open || self.tab_close_confirm.is_some() {
            return None;
        }
        let controller = self.app_update.as_ref()?;
        let snapshot = controller.snapshot();
        let Some(model) = prompt_model(&snapshot) else {
            self.app_update_prompt_reported.borrow_mut().take();
            return None;
        };
        let mut reported = self.app_update_prompt_reported.borrow_mut();
        if reported.as_deref() != Some(model.candidate_id.as_str()) {
            controller.prompt_shown(&model.candidate_id);
            *reported = Some(model.candidate_id.clone());
        }
        drop(reported);
        let border = rgb(self.theme.sidebar_active_background);
        let restart_id = model.candidate_id.clone();
        let later_id = model.candidate_id;
        let save_first = self.save_and_restart_targets(&snapshot, cx).is_some();
        let restart_button = (!self.update_restart_busy()).then(|| {
            let (id, label) = if save_first {
                ("app-update-prompt-save-restart", "保存して再起動")
            } else {
                ("app-update-prompt-restart", "再起動して更新")
            };
            Self::update_prompt_button(
                cx,
                border,
                id,
                label,
                &self.app_update_restart_focus,
                move |view, cx| {
                    view.restart_message = None;
                    if save_first {
                        view.start_save_and_restart(&restart_id, cx);
                    } else {
                        view.start_update_restart(Some(&restart_id), cx);
                    }
                },
            )
        });
        let later_button = Self::update_prompt_button(
            cx,
            border,
            "app-update-prompt-later",
            "後で",
            &self.app_update_later_focus,
            move |view, cx| view.dismiss_update_prompt(&later_id, cx),
        );
        let notice = self.restart_message.clone().map(|message| {
            div()
                .id("app-update-prompt-message")
                .text_color(rgb(self.theme.quote_foreground))
                .child(message)
        });
        Some(
            div()
                .id("app-update-prompt")
                .debug_selector(|| "app-update-prompt".to_owned())
                .absolute()
                .top_0()
                .left_0()
                .right_0()
                .occlude()
                // The editor body listens for presses; a press on the band is
                // not a click into the document.
                .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .px(px(16.0))
                .py(px(8.0))
                .flex()
                .flex_row()
                .flex_wrap()
                .items_center()
                .gap_3()
                .bg(rgb(self.theme.code_background))
                .border_b_1()
                .border_color(border)
                .text_color(rgb(self.theme.foreground))
                .child(div().child(model.message))
                .children(notice)
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .gap_2()
                        .children(restart_button)
                        .child(later_button),
                )
                .into_any_element(),
        )
    }

    /// The "アプリの更新" card of the general settings page. `None` when no
    /// update service is attached (tests, unsupported builds).
    pub(super) fn app_update_card(&self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        let controller = self.app_update.as_ref()?;
        let snapshot = controller.snapshot();
        let model = card_model(&snapshot);
        let restarting = self.update_restart_busy();
        let save_first = self.save_and_restart_targets(&snapshot, cx).is_some();
        let candidate = snapshot.candidate_id.clone();
        let buttons: Vec<Button> = model
            .actions
            .into_iter()
            .map(|(action, label)| {
                if save_first
                    && action == CardAction::Restart
                    && let Some(candidate) = candidate.clone()
                {
                    (CardAction::SaveAndRestart(candidate), "保存して再起動")
                } else {
                    (action, label)
                }
            })
            .filter(|(action, _)| {
                !(restarting
                    && matches!(action, CardAction::Restart | CardAction::SaveAndRestart(_)))
            })
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
        // Available whenever records exist, also while updates are
        // unavailable; it only copies local text and starts nothing.
        let diagnostics_button = snapshot.diagnostics_available.then(|| {
            let view = cx.entity();
            div()
                .id("settings-app-update-diagnostics-wrapper")
                .debug_selector(|| "settings-app-update-diagnostics".to_owned())
                .child(
                    Button::new("settings-app-update-diagnostics")
                        .label("診断情報をコピー")
                        .ghost()
                        .on_click(move |_, _window, app| {
                            view.update(app, |view, cx| view.copy_app_update_diagnostics(cx));
                        }),
                )
        });
        let auto_view = cx.entity();
        let auto_toggle = Checkbox::new("settings-app-update-auto")
            .checked(self.settings.auto_update_check)
            .label("自動で更新を確認する")
            .on_click(move |checked, _window, app| {
                auto_view.update(app, |view, cx| view.set_auto_update_check(*checked, cx));
            });
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
            .children(
                self.update_completed
                    .clone()
                    .filter(|_| self.explorer_migration_failed.is_none())
                    .map(|version| {
                        div()
                            .id("settings-app-update-completed")
                            .debug_selector(|| "settings-app-update-completed".to_owned())
                            .child(format!("Hane {version} に更新しました。"))
                    }),
            )
            .children(self.explorer_migration_failed.map(|scope| {
                div()
                    .id("settings-app-update-explorer-failed")
                    .debug_selector(|| "settings-app-update-explorer-failed".to_owned())
                    .child(explorer_migration_failed_message(scope))
            }))
            .children(self.restart_message.clone().map(|message| {
                div()
                    .id("settings-app-update-restart-message")
                    .text_color(rgb(self.theme.quote_foreground))
                    .child(message)
            }))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .gap_2()
                    .children(buttons)
                    .children(diagnostics_button),
            )
            .child(auto_toggle)
            .child(
                div()
                    .id("settings-app-update-auto-description")
                    .text_color(rgb(self.theme.quote_foreground))
                    .child("起動後と6時間ごとに確認し、更新をダウンロードします。適用前に確認します。"),
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
        self.submit_external_open_protected(path, false, cx)
    }

    /// [`Self::submit_external_open`] for a request that waited in a queue:
    /// `protected` is the unsaved / saving state seen when it was received.
    pub fn submit_external_open_protected(
        &mut self,
        path: &Path,
        protected: bool,
        cx: &mut Context<Self>,
    ) -> ExternalOpenOutcome {
        if self.restart_freeze {
            return ExternalOpenOutcome::Busy;
        }
        self.external_open_generation = self.external_open_generation.wrapping_add(1);
        self.open_external_path_protected(path, protected, cx);
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
        #[cfg(test)]
        if self.restart_test_ai_busy {
            blockers.push(RestartBlocker::AiBusy);
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
        if self.network_settings.is_dirty(cx) {
            blockers.push(RestartBlocker::NetworkFormUnsaved);
        }
        if self.file_context_menu_busy || self.network_settings.is_busy() {
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

    /// Installs the explicit stop/restart handle of the app-owned AI service.
    pub fn attach_ai_lifecycle(&mut self, lifecycle: Option<Arc<dyn AiLifecycle>>) {
        self.ai_lifecycle = lifecycle;
    }

    /// Shows, once, that this process is the version an update just installed.
    /// The caller passes the version only after the helper's durable commit.
    pub fn show_update_completed(&mut self, version: &str, cx: &mut Context<Self>) {
        self.update_completed = Some(version.to_owned());
        cx.notify();
    }

    /// Tells the user that Hane itself was updated but the Explorer
    /// integration was not carried over, and how to repair it. Never shown for
    /// a successful migration.
    pub fn report_explorer_migration_failed(
        &mut self,
        scope: ExplorerRepairScope,
        cx: &mut Context<Self>,
    ) {
        self.apply_explorer_migration_failed(scope);
        cx.notify();
    }

    /// The state change of [`Self::report_explorer_migration_failed`]. The
    /// ordinary "updated" notice is dropped so only the partial failure shows;
    /// the update itself and the restored documents are untouched.
    fn apply_explorer_migration_failed(&mut self, scope: ExplorerRepairScope) {
        self.update_completed = None;
        self.explorer_migration_failed = Some(scope);
        self.status = Some(explorer_migration_failed_message(scope));
    }

    /// Tells the user that some files open before the update restart could not
    /// be reopened because they were deleted or moved.
    pub fn report_restore_skipped(&mut self, count: usize, cx: &mut Context<Self>) {
        self.status = Some(format!(
            "再起動前に開いていた {count} 件のファイルが見つからないため開けませんでした"
        ));
        cx.notify();
    }

    /// The saved files of this window for the restart ticket. Unsaved
    /// documents are not part of it (a restart is blocked while any exist).
    pub(super) fn restart_ticket_snapshot(&self) -> RestartTicketSnapshot {
        let active_id = self.sessions.active_id();
        let mut files = Vec::new();
        let mut active = None;
        for session in self.sessions.sessions() {
            let Some(path) = session.path() else {
                continue;
            };
            if session.is_dirty() {
                continue;
            }
            if session.id() == active_id {
                active = Some(files.len());
            }
            files.push(RestartTicketFile {
                path: path.to_path_buf(),
                scroll_line: 0,
                caret_offset: 0,
            });
        }
        RestartTicketSnapshot {
            work_folder: self
                .work_folder
                .as_ref()
                .map(|folder| folder.root().to_path_buf()),
            files,
            active,
        }
    }

    /// "再起動して更新". Every blocker is checked first; the helper is then
    /// prepared in the background and the exit is decided only when it has
    /// answered `Prepared`.
    ///
    /// `expected` is the candidate a button was rendered for; when the service
    /// has moved on to another one, nothing starts.
    fn start_update_restart(&mut self, expected: Option<&str>, cx: &mut Context<Self>) {
        if self.update_restart_busy() {
            return;
        }
        let Some(controller) = self.app_update.clone() else {
            return;
        };
        let snapshot = controller.snapshot();
        if let Some(expected) = expected
            && snapshot.candidate_id.as_deref() != Some(expected)
        {
            return;
        }
        let (AppUpdatePhase::Ready, true, Some(candidate)) = (
            snapshot.phase,
            snapshot.restart_supported,
            snapshot.candidate_id.clone(),
        ) else {
            return;
        };
        self.restart_message = None;
        let observation = match self.prepare_update_restart(cx) {
            RestartPreparation::Clear(observation) => observation,
            RestartPreparation::Blocked(blockers) => {
                let reasons: Vec<String> = blockers.iter().map(RestartBlocker::message).collect();
                self.restart_message = Some(reasons.join(" "));
                cx.notify();
                return;
            }
        };
        let ticket = self.restart_ticket_snapshot();
        self.restart_in_progress = true;
        self.restart_message = Some("再起動の準備をしています…".to_owned());
        cx.notify();
        cx.spawn(async move |view, cx| {
            let prepared = cx
                .background_executor()
                .spawn(async move { controller.prepare_restart(&candidate) })
                .await;
            let _ = view.update(cx, |view, cx| {
                view.continue_update_restart(prepared, observation, ticket, cx);
            });
        })
        .detach();
    }

    /// With the restart freeze already held, re-runs the full blocker check
    /// (documents, sessions, settings/prompt forms, IME, save, rename, open,
    /// AI operations, persistence) and compares against the first observation.
    /// Nothing is stopped or written to the helper here.
    fn recheck_restart_under_freeze(
        &mut self,
        observation: &RestartObservation,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        match self.prepare_update_restart_frozen(cx) {
            RestartPreparation::Blocked(blockers) => {
                let reasons: Vec<String> = blockers.iter().map(RestartBlocker::message).collect();
                Err(reasons.join(" "))
            }
            RestartPreparation::Clear(_) => {
                if self.restart_observation_is_current(observation) {
                    Ok(())
                } else {
                    Err(RESTART_STALE_MESSAGE.to_owned())
                }
            }
        }
    }

    fn keep_running_after_restart_attempt(&mut self, message: String, cx: &mut Context<Self>) {
        self.end_restart_freeze();
        self.restart_in_progress = false;
        self.restart_message = Some(message);
        cx.notify();
    }

    /// Runs on the UI thread once the helper preparation finished. The exit is
    /// authorized only by [`run_exit_sequence`] and only after a `Prepared`.
    fn continue_update_restart(
        &mut self,
        prepared: Result<Box<dyn PreparedUpdate>, String>,
        observation: RestartObservation,
        ticket: RestartTicketSnapshot,
        cx: &mut Context<Self>,
    ) {
        let prepared = match prepared {
            Ok(prepared) => prepared,
            Err(reason) => {
                self.keep_running_after_restart_attempt(
                    format!("更新を開始できませんでした。アプリはそのまま使えます: {reason}"),
                    cx,
                );
                return;
            }
        };
        if !self.begin_restart_freeze() {
            prepared.abort();
            self.restart_in_progress = false;
            self.restart_message = Some("再起動の準備中です。".to_owned());
            cx.notify();
            return;
        }
        // The window was editable while the helper prepared. With the freeze
        // held, look at every blocker again and at whether anything the first
        // check observed has moved, before a ticket or the AI stop.
        if let Err(message) = self.recheck_restart_under_freeze(&observation, cx) {
            prepared.abort();
            self.keep_running_after_restart_attempt(message, cx);
            return;
        }
        let ai = self.ai_lifecycle.clone();
        let outcome = {
            let mut steps = ViewExit {
                view: &mut *self,
                cx: &mut *cx,
                prepared: Some(prepared),
                observation: &observation,
                ticket: &ticket,
                ai,
                ai_stopped: false,
            };
            run_exit_sequence(&mut steps)
        };
        match outcome {
            ExitOutcome::Quit => {
                // The freeze stays held: a request arriving now is answered
                // `Busy` and kept by its sender instead of being lost.
                cx.quit();
            }
            ExitOutcome::Kept(message) => {
                self.keep_running_after_restart_attempt(message, cx);
            }
        }
    }
}

/// What `save_session_tracked` did with one save request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SaveStart {
    /// A write was started; its completion is reported for this ticket.
    Started(SaveTicket),
    /// A write was already running; the request was queued behind it.
    Queued,
    /// The document has no path.
    NeedsPath,
    /// A confirmed overwrite no longer matched.
    ApprovalExpired,
    /// A folder move to the Trash refused the write.
    Refused,
    /// The session does not exist any more.
    Gone,
}

impl SaveStart {
    fn reason(self) -> &'static str {
        match self {
            Self::Started(_) => "保存を開始しました",
            Self::Queued => "別の保存が進行中のため、保存を開始できませんでした",
            Self::NeedsPath => "保存先が決まっていません",
            Self::ApprovalExpired => "確認後に文書または外部のファイルが変更されました",
            Self::Refused => "フォルダをゴミ箱へ移動中のため保存できません",
            Self::Gone => "文書が閉じられました",
        }
    }
}

/// How a started write ended, as reported once to the batch that waits for it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum TrackedSaveResult {
    Saved,
    SavedStale,
    Conflict,
    Failed(String),
    Superseded,
}

impl TrackedSaveResult {
    pub(crate) fn from_outcome(outcome: &SaveOutcome) -> Self {
        match outcome {
            SaveOutcome::Saved => Self::Saved,
            SaveOutcome::SavedStale => Self::SavedStale,
            SaveOutcome::Conflict => Self::Conflict,
            SaveOutcome::Failed(error) => Self::Failed(error.to_string()),
            SaveOutcome::Superseded => Self::Superseded,
        }
    }

    fn reason(&self) -> String {
        match self {
            Self::Saved => "保存しました".to_owned(),
            Self::SavedStale => {
                "保存中に文書が編集されました。編集内容は保持しています".to_owned()
            }
            Self::Conflict => {
                "保存先のファイルが外部で変更されています。上書きは行っていません".to_owned()
            }
            Self::Failed(error) => error.clone(),
            Self::Superseded => "保存の対象の文書が変わりました".to_owned(),
        }
    }
}

#[derive(Clone, Debug)]
struct UpdateSaveTarget {
    id: SessionId,
    label: String,
    path: PathBuf,
}

/// One session as the batch saw it when it started.
#[derive(Clone, Debug, Eq, PartialEq)]
struct UpdateSaveSession {
    id: SessionId,
    instance: Option<DocumentInstance>,
    revision: Revision,
    path: Option<PathBuf>,
}

/// The "save and restart" operation: it writes every unsaved, file-backed
/// document one at a time and only then joins `start_update_restart`.
#[derive(Debug)]
pub(crate) struct UpdateSaveBatch {
    candidate_id: String,
    workspace_generation: u64,
    /// Every session at the start; any difference aborts the batch.
    sessions: Vec<UpdateSaveSession>,
    /// The dirty documents, in `SessionId` order.
    targets: Vec<UpdateSaveTarget>,
    /// Index of the target being written, or the next one to write.
    next: usize,
    /// The one write whose completion is being waited for.
    awaiting: Option<(SessionId, SaveTicket)>,
}

fn save_failure_message(target: &UpdateSaveTarget, reason: &str) -> String {
    format!(
        "「{}」を保存できなかったため、更新を中止しました。\n保存先: {}\n理由: {}",
        target.label,
        target.path.display(),
        reason
    )
}

impl EditorView {
    fn update_save_sessions(&self) -> Vec<UpdateSaveSession> {
        self.sessions
            .sessions()
            .map(|session| UpdateSaveSession {
                id: session.id(),
                instance: self.document_instance(session.id()),
                revision: session.revision(),
                path: session.path().map(Path::to_path_buf),
            })
            .collect()
    }

    /// The sessions "保存して再起動" would save, or `None` when it must not be
    /// offered. Side-effect free, so it is safe to call while drawing: it never
    /// runs `prepare_update_restart` (which writes drafts and settings).
    ///
    /// Offered only when the update is restartable, nothing is running, the only
    /// restart blockers are unsaved documents, and every unsaved document has a
    /// file path. An edited untitled note stays dirty even after its draft was
    /// written, so one such note withholds the action.
    pub(super) fn save_and_restart_targets(
        &self,
        snapshot: &AppUpdateSnapshot,
        cx: &mut Context<Self>,
    ) -> Option<Vec<SessionId>> {
        if snapshot.phase != AppUpdatePhase::Ready
            || !snapshot.restart_supported
            || snapshot.restoring
            || snapshot.candidate_id.is_none()
        {
            return None;
        }
        if self.restart_in_progress || self.update_save_batch.is_some() || self.restart_freeze {
            return None;
        }
        let dirty: Vec<&DocumentSession> = self
            .sessions
            .sessions()
            .filter(|session| session.is_dirty())
            .collect();
        if dirty.is_empty() || dirty.iter().any(|session| session.path().is_none()) {
            return None;
        }
        if !self
            .restart_blockers(cx)
            .iter()
            .all(|blocker| matches!(blocker, RestartBlocker::DirtyDocument(_)))
        {
            return None;
        }
        let mut ids: Vec<SessionId> = dirty.iter().map(|session| session.id()).collect();
        ids.sort();
        Some(ids)
    }

    /// True while a restart or a save-and-restart batch owns the action.
    pub(super) fn update_restart_busy(&self) -> bool {
        self.restart_in_progress || self.update_save_batch.is_some()
    }

    /// "保存して再起動". `expected` is the candidate the button was drawn for.
    /// The batch is registered before the first write starts, in this same UI
    /// turn, so a second activation finds it and does nothing.
    pub(super) fn start_save_and_restart(&mut self, expected: &str, cx: &mut Context<Self>) {
        if self.update_restart_busy() {
            return;
        }
        let Some(controller) = self.app_update.clone() else {
            return;
        };
        let snapshot = controller.snapshot();
        if snapshot.candidate_id.as_deref() != Some(expected) {
            return;
        }
        self.restart_message = None;
        let Some(ids) = self.save_and_restart_targets(&snapshot, cx) else {
            // The situation changed since the button was drawn: the normal
            // path reports whatever blocks the restart now.
            self.start_update_restart(Some(expected), cx);
            return;
        };
        let targets: Vec<UpdateSaveTarget> = ids
            .iter()
            .filter_map(|id| {
                let session = self.sessions.get(*id)?;
                Some(UpdateSaveTarget {
                    id: *id,
                    label: session.label(),
                    path: session.path()?.to_path_buf(),
                })
            })
            .collect();
        if targets.len() != ids.len() {
            self.start_update_restart(Some(expected), cx);
            return;
        }
        self.update_save_batch = Some(UpdateSaveBatch {
            candidate_id: expected.to_owned(),
            workspace_generation: self.work_folder_generation,
            sessions: self.update_save_sessions(),
            targets,
            next: 0,
            awaiting: None,
        });
        self.continue_update_save(cx);
    }

    /// Why the batch cannot go on, or `None`. Anything that changed since the
    /// batch started (documents, paths, workspace, candidate, other blockers)
    /// is a reason; the remaining targets' unsaved state is the one allowed
    /// difference until `finished`.
    fn update_save_problem(
        &self,
        batch: &UpdateSaveBatch,
        finished: bool,
        cx: &mut Context<Self>,
    ) -> Option<String> {
        let label_of = |id: SessionId| {
            self.sessions
                .get(id)
                .map(DocumentSession::label)
                .unwrap_or_default()
        };
        let candidate_current = self.app_update.as_ref().is_some_and(|controller| {
            let snapshot = controller.snapshot();
            snapshot.phase == AppUpdatePhase::Ready
                && snapshot.restart_supported
                && snapshot.candidate_id.as_deref() == Some(batch.candidate_id.as_str())
        });
        if !candidate_current {
            return Some("更新の候補が変わったため、更新を中止しました。".to_owned());
        }
        if self.work_folder_generation != batch.workspace_generation {
            return Some("作業フォルダーが切り替わったため、更新を中止しました。".to_owned());
        }
        let now = self.update_save_sessions();
        if now.len() != batch.sessions.len()
            || now.iter().zip(&batch.sessions).any(|(a, b)| a.id != b.id)
        {
            return Some("開いている文書の構成が変わったため、更新を中止しました。".to_owned());
        }
        for (current, started) in now.iter().zip(&batch.sessions) {
            let label = label_of(current.id);
            if current.instance != started.instance {
                return Some(format!(
                    "「{label}」が別の文書に置き換わったため、更新を中止しました。"
                ));
            }
            if current.path != started.path {
                let path = current
                    .path
                    .as_ref()
                    .map(|path| path.display().to_string())
                    .unwrap_or_default();
                return Some(format!(
                    "「{label}」の保存先が変更されたため、更新を中止しました。\n保存先: {path}"
                ));
            }
            if current.revision != started.revision {
                return Some(format!(
                    "「{label}」が編集されたため、更新を中止しました。編集内容は保持しています。"
                ));
            }
        }
        let remaining = &batch.targets[batch.next.min(batch.targets.len())..];
        for session in self.sessions.sessions() {
            let expected = remaining.iter().any(|target| target.id == session.id());
            if session.is_dirty() != expected {
                return Some(format!(
                    "「{}」の未保存の状態が変わったため、更新を中止しました。",
                    session.label()
                ));
            }
        }
        let reasons: Vec<String> = self
            .restart_blockers(cx)
            .iter()
            .filter(|blocker| finished || !matches!(blocker, RestartBlocker::DirtyDocument(_)))
            .map(RestartBlocker::message)
            .collect();
        if !reasons.is_empty() {
            return Some(format!(
                "他の操作が行われたため、更新を中止しました。{}",
                reasons.join(" ")
            ));
        }
        None
    }

    /// Writes the next target, or, when every target was saved, joins the
    /// normal restart path. The batch is held outside `self` only for the
    /// duration of this call.
    fn continue_update_save(&mut self, cx: &mut Context<Self>) {
        let Some(mut batch) = self.update_save_batch.take() else {
            return;
        };
        let finished = batch.next >= batch.targets.len();
        if let Some(problem) = self.update_save_problem(&batch, finished, cx) {
            self.restart_message = Some(problem);
            cx.notify();
            return;
        }
        if finished {
            // The batch is gone; the existing entrance re-observes everything
            // and takes its own ticket, so nothing from before the saves is
            // reused.
            self.restart_message = None;
            self.start_update_restart(Some(&batch.candidate_id), cx);
            return;
        }
        let target = batch.targets[batch.next].clone();
        let total = batch.targets.len();
        match self.save_session_tracked(target.id, SaveIntent::Current, cx) {
            SaveStart::Started(ticket) => {
                batch.awaiting = Some((target.id, ticket));
                self.restart_message =
                    Some(format!("保存しています…（{}／{}）", batch.next, total));
                self.update_save_batch = Some(batch);
            }
            other => {
                self.restart_message = Some(save_failure_message(&target, other.reason()));
            }
        }
        cx.notify();
    }

    /// Called by `finish_save` on every path once the write of `ticket` has
    /// been fully processed. Only the write the batch waits for counts, and
    /// only `Saved` lets it go on.
    pub(super) fn report_tracked_save(
        &mut self,
        id: SessionId,
        ticket: SaveTicket,
        result: TrackedSaveResult,
        cx: &mut Context<Self>,
    ) {
        let Some(batch) = self.update_save_batch.as_mut() else {
            return;
        };
        if batch.awaiting != Some((id, ticket)) {
            return;
        }
        batch.awaiting = None;
        if result == TrackedSaveResult::Saved {
            batch.next += 1;
            self.continue_update_save(cx);
            return;
        }
        let target = batch.targets.get(batch.next).cloned();
        self.update_save_batch = None;
        self.restart_message = Some(match target {
            Some(target) => save_failure_message(&target, &result.reason()),
            None => result.reason(),
        });
        cx.notify();
    }
}

const RESTART_STALE_MESSAGE: &str =
    "文書または設定が変更されたため、更新を中止しました。もう一度お試しください。";

/// How long the AI service may take to stop before the restart is given up.
pub(crate) const AI_STOP_TIMEOUT: Duration = Duration::from_secs(10);

/// How often a stop that outlived [`AI_STOP_TIMEOUT`] is checked for completion.
/// Shown when the update committed but the Explorer integration was not carried
/// over. The new publisher is never trusted automatically; the user repairs it
/// from the settings.
/// The partial-failure notice with repair steps for only the registrations
/// that were enabled before the update. A disabled kind is never mentioned, so
/// the user is not led to turn it on.
fn explorer_migration_failed_message(scope: ExplorerRepairScope) -> String {
    let mut message = String::from(
        "Hane本体は更新しましたが、Explorer連携の更新に失敗しました。",
    );
    if scope.file {
        message.push_str(
            "ファイルの右クリック登録は、設定画面の「ファイルを右クリックでHaneで開けるよう登録」をオフにしてから、もう一度オンにして修復してください。",
        );
    }
    if scope.folder {
        message.push_str(
            "フォルダーの右クリック登録は、Haneを「--register-context-menu」付きで実行して再登録してください。",
        );
    }
    message.push_str(
        "発行元が変わって再登録できない場合は、アプリが自動で信頼することはないため、発行元を確認したうえで利用者が明示的に信頼してください。",
    );
    message
}

const AI_RESUME_POLL_INTERVAL: Duration = Duration::from_millis(200);

/// The steps of the final exit coordination, behind a seam so the order and
/// every failure path can be tested without a window or a helper process.
pub(crate) trait ExitSteps {
    /// The restart permit (every session identity/revision and generation of
    /// the check) is still current.
    fn permit_is_fresh(&self) -> bool;
    fn issue_ticket(&mut self) -> Result<(), String>;
    /// Writes every pending draft and reports any failure.
    fn drain_drafts(&mut self) -> Result<(), String>;
    fn stop_ai(&mut self) -> Result<(), String>;
    /// Closes the external-open queue atomically with its emptiness check.
    /// `false` means a request is waiting and the restart must be given up.
    /// From here on a request is refused by the queue and handed to the next
    /// process by the sender, never queued where this process cannot open it.
    fn seal_open_queue(&mut self) -> bool;
    /// Reopens the queue after the restart was given up.
    fn unseal_open_queue(&mut self);
    /// Lets the helper proceed. After this returns `Ok` the caller quits.
    fn authorize_exit(&mut self) -> Result<(), String>;
    fn abort_helper(&mut self);
    fn resume_ai(&mut self);
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum ExitOutcome {
    /// The helper was authorized; the caller quits normally.
    Quit,
    /// The restart was given up. The helper was aborted, the AI service
    /// started again if it had been stopped, and nothing was lost.
    Kept(String),
}

/// Quit coordination (spec section 7, steps 6-8). Nothing here may change the
/// install: the helper does that only after `authorize_exit` and this
/// process's exit. Any failure aborts the helper and keeps the old window.
pub(crate) fn run_exit_sequence(steps: &mut dyn ExitSteps) -> ExitOutcome {
    const STALE: &str = RESTART_STALE_MESSAGE;
    if !steps.permit_is_fresh() {
        steps.abort_helper();
        return ExitOutcome::Kept(STALE.to_owned());
    }
    if let Err(reason) = steps.issue_ticket() {
        steps.abort_helper();
        return ExitOutcome::Kept(format!("再起動の準備に失敗しました: {reason}"));
    }
    if let Err(reason) = steps.drain_drafts() {
        steps.abort_helper();
        return ExitOutcome::Kept(format!("下書きを書き込めなかったため中止しました: {reason}"));
    }
    if let Err(reason) = steps.stop_ai() {
        steps.abort_helper();
        steps.resume_ai();
        return ExitOutcome::Kept(format!("AIサービスを停止できなかったため中止しました: {reason}"));
    }
    if !steps.permit_is_fresh() {
        steps.abort_helper();
        steps.resume_ai();
        return ExitOutcome::Kept(STALE.to_owned());
    }
    if !steps.seal_open_queue() {
        steps.abort_helper();
        steps.resume_ai();
        return ExitOutcome::Kept(STALE.to_owned());
    }
    if let Err(reason) = steps.authorize_exit() {
        // The handle is consumed (and the helper aborted) by the failure.
        steps.unseal_open_queue();
        steps.resume_ai();
        return ExitOutcome::Kept(format!("更新ヘルパーへの終了許可に失敗しました: {reason}"));
    }
    ExitOutcome::Quit
}

/// [`ExitSteps`] on the real window.
struct ViewExit<'a, 'c> {
    view: &'a mut EditorView,
    cx: &'a mut Context<'c, EditorView>,
    prepared: Option<Box<dyn PreparedUpdate>>,
    observation: &'a RestartObservation,
    ticket: &'a RestartTicketSnapshot,
    ai: Option<Arc<dyn AiLifecycle>>,
    ai_stopped: bool,
}

impl ExitSteps for ViewExit<'_, '_> {
    fn permit_is_fresh(&self) -> bool {
        // A document that reached the queue after the ticket was written is
        // not in the ticket and the old process cannot hand it over. Keep
        // running; the request stays queued and opens once the freeze ends.
        self.view.restart_observation_is_current(self.observation)
            && !self.view.external_open_waiting()
    }

    fn issue_ticket(&mut self) -> Result<(), String> {
        let Some(prepared) = self.prepared.as_mut() else {
            return Err("更新ヘルパーがありません".to_owned());
        };
        let view = &*self.view;
        let observation = self.observation;
        prepared.issue_ticket(self.ticket, &|| view.restart_observation_is_current(observation))
    }

    fn drain_drafts(&mut self) -> Result<(), String> {
        self.view
            .flush_pending_drafts_checked()
            .map(|_| ())
            .map_err(|failures| failures.join("; "))
    }

    fn stop_ai(&mut self) -> Result<(), String> {
        let Some(ai) = self.ai.clone() else {
            return Ok(());
        };
        self.ai_stopped = true;
        ai.stop(AI_STOP_TIMEOUT)
    }

    fn seal_open_queue(&mut self) -> bool {
        self.view.seal_external_open()
    }

    fn unseal_open_queue(&mut self) {
        self.view.unseal_external_open();
    }

    fn authorize_exit(&mut self) -> Result<(), String> {
        match self.prepared.take() {
            Some(prepared) => prepared.authorize_exit(),
            None => Err("更新ヘルパーがありません".to_owned()),
        }
    }

    fn abort_helper(&mut self) {
        if let Some(prepared) = self.prepared.take() {
            prepared.abort();
        }
    }

    fn resume_ai(&mut self) {
        if !self.ai_stopped {
            return;
        }
        // The service is started again and the window gets its new handle; a
        // service that cannot be started leaves the AI features unavailable
        // until the next launch, but the editor itself is unaffected.
        let outcome = self.ai.as_ref().map(|ai| ai.resume());
        let (handle, still_stopping) = match outcome {
            Some(AiResume::Started(handle)) => (Some(handle), false),
            Some(AiResume::Stopping) => (None, true),
            Some(AiResume::Failed) | None => (None, false),
        };
        self.view.attach_ai_service(handle, self.cx);
        self.ai_stopped = false;
        if still_stopping && let Some(ai) = self.ai.clone() {
            spawn_ai_resume_wait(ai, self.cx);
        }
    }
}

/// The old service is still stopping, so the owner keeps the start pending and
/// runs it once the stop completes. Poll only the state of that one pending
/// start and attach the new handle instead of leaving the AI detached. There is
/// no poll limit, because a stop can finish arbitrarily late; the wait ends
/// when the handle is attached, when the start failed, or when the view is gone.
fn spawn_ai_resume_wait(ai: Arc<dyn AiLifecycle>, cx: &mut Context<EditorView>) {
    cx.spawn(async move |view, cx| {
        loop {
            cx.background_executor().timer(AI_RESUME_POLL_INTERVAL).await;
            // A released view ends the wait; nothing is left to attach to.
            if view.update(cx, |_, _| ()).is_err() {
                return;
            }
            match ai.poll_resume() {
                AiResume::Started(handle) => {
                    let _ = view.update(cx, |view, cx| {
                        view.attach_ai_service(Some(handle), cx);
                    });
                    return;
                }
                AiResume::Stopping => {}
                AiResume::Failed => return,
            }
        }
    })
    .detach();
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

    #[test]
    fn the_partial_failure_notice_repairs_only_the_registrations_that_were_enabled() {
        const FILE_STEP: &str = "ファイルを右クリックでHaneで開けるよう登録";
        const FOLDER_STEP: &str = "--register-context-menu";
        for (file, folder) in [(true, false), (false, true), (true, true)] {
            let text = explorer_migration_failed_message(ExplorerRepairScope { file, folder });
            assert!(text.contains("Hane本体は更新しました"));
            assert!(text.contains("Explorer連携の更新に失敗"));
            assert_eq!(text.contains(FILE_STEP), file, "{text}");
            assert_eq!(text.contains(FOLDER_STEP), folder, "{text}");
            assert!(text.contains("発行元"));
            assert!(text.contains("明示的に信頼"));
            // It is not the plain success notice.
            assert!(!text.contains("に更新しました。"));
        }
    }

    #[gpui::test]
    fn a_partial_failure_replaces_the_plain_updated_notice(cx: &mut gpui::TestAppContext) {
        let controller = FakeController::new(AppUpdateSnapshot::idle("0.26.0"));
        let (view, cx) = cx.add_window_view(|_, cx| EditorView::new("body\n", "Untitled", cx));
        let attached: Arc<dyn AppUpdateController> = controller.clone();
        view.update(cx, |view, cx| {
            view.attach_app_update(Some(attached));
            view.show_update_completed("0.27.0", cx);
        });
        open_general_settings(&view, cx);
        assert!(cx.debug_bounds("settings-app-update-completed").is_some());
        assert!(
            cx.debug_bounds("settings-app-update-explorer-failed")
                .is_none()
        );

        let scope = ExplorerRepairScope {
            file: false,
            folder: true,
        };
        view.update(cx, |view, cx| {
            view.report_explorer_migration_failed(scope, cx);
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds("settings-app-update-completed").is_none());
        assert!(
            cx.debug_bounds("settings-app-update-explorer-failed")
                .is_some()
        );
        view.update(cx, |view, _| {
            assert_eq!(view.update_completed, None);
            assert_eq!(
                view.status.as_deref(),
                Some(explorer_migration_failed_message(scope).as_str())
            );
        });
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
    fn an_unusable_placement_explains_itself_without_promising_a_fix() {
        for code in ["app_translocated", "unsupported_install", "install_modified", "io"] {
            let mut input = snapshot(AppUpdatePhase::Unavailable);
            input.error_code = Some(code.to_owned());
            let model = card_model(&input);
            assert!(model.actions.is_empty(), "{code}");
            let detail = model.detail.unwrap();
            assert!(detail.contains(code), "{code}: {detail}");
            assert!(!detail.contains("更新情報を利用できません"), "{code}: {detail}");
            assert!(!detail.contains("必ず"), "{code}: {detail}");
        }
    }

    #[test]
    fn an_owner_mismatch_placement_explains_the_owner_and_a_safe_action() {
        let mut input = snapshot(AppUpdatePhase::Unavailable);
        input.error_code = Some("identity_mismatch".to_owned());
        let model = card_model(&input);
        assert!(model.actions.is_empty());
        let detail = model.detail.unwrap();
        assert!(detail.contains("所有者"), "{detail}");
        assert!(detail.contains("入れ直"), "{detail}");
        assert!(detail.contains("identity_mismatch"), "{detail}");
        assert!(!detail.contains("更新情報を利用できません"), "{detail}");
        assert!(!detail.contains("署名"), "{detail}");
        assert!(!detail.contains("管理者"), "{detail}");
    }

    #[test]
    fn a_failed_identity_mismatch_keeps_the_signature_wording() {
        let mut input = snapshot(AppUpdatePhase::Failed);
        input.error_code = Some("identity_mismatch".to_owned());
        let model = card_model(&input);
        let detail = model.detail.clone().unwrap();
        assert!(detail.contains("署名または発行元"), "{detail}");
        assert!(!detail.contains("所有者"), "{detail}");
        assert_eq!(labels(&model), ["再試行"]);
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
    fn download_stage_integrity_and_limit_failures_explain_the_download() {
        for code in [
            "hash_mismatch",
            "size_mismatch",
            "limit_exceeded",
            "redirect_rejected",
        ] {
            let mut input = snapshot(AppUpdatePhase::Failed);
            input.error_code = Some(code.to_owned());
            input.error_stage = Some("download".to_owned());
            let detail = card_model(&input).detail.unwrap();
            assert!(detail.contains("ダウンロード"), "{code}: {detail}");
            assert!(!detail.contains("更新を確認できませんでした"), "{code}: {detail}");
            assert!(detail.contains(code), "{code}: {detail}");
        }
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
    fn an_unsupported_build_can_only_discard_a_ready_update() {
        let model = card_model(&snapshot(AppUpdatePhase::Ready));
        assert_eq!(labels(&model), ["準備した更新を削除"]);
    }

    #[test]
    fn restart_is_offered_only_for_a_ready_update_of_a_supporting_service() {
        for phase in [
            AppUpdatePhase::Idle,
            AppUpdatePhase::Checking,
            AppUpdatePhase::UpToDate,
            AppUpdatePhase::Available,
            AppUpdatePhase::Downloading,
            AppUpdatePhase::Verifying,
            AppUpdatePhase::Discarding,
            AppUpdatePhase::Failed,
            AppUpdatePhase::Unavailable,
        ] {
            let mut input = snapshot(phase);
            input.restart_supported = true;
            let model = card_model(&input);
            assert!(
                !model.actions.iter().any(|(action, _)| *action == CardAction::Restart),
                "{phase:?}"
            );
        }
        let mut input = snapshot(AppUpdatePhase::Ready);
        input.restart_supported = true;
        assert_eq!(
            labels(&card_model(&input)),
            ["再起動して更新", "準備した更新を削除"]
        );
        input.restoring = true;
        assert!(card_model(&input).actions.is_empty());
    }

    #[derive(Default)]
    struct FakeExit {
        calls: Vec<&'static str>,
        stale_after: Option<usize>,
        permit_checks: std::cell::Cell<usize>,
        fail: Option<&'static str>,
        /// A request reached the queue before it was sealed.
        open_waiting_at_seal: bool,
    }

    impl FakeExit {
        fn failing(step: &'static str) -> Self {
            Self {
                fail: Some(step),
                ..Self::default()
            }
        }

        fn step(&mut self, name: &'static str) -> Result<(), String> {
            self.calls.push(name);
            if self.fail == Some(name) {
                Err(format!("{name} failed"))
            } else {
                Ok(())
            }
        }
    }

    impl ExitSteps for FakeExit {
        fn permit_is_fresh(&self) -> bool {
            let seen = self.permit_checks.get();
            self.permit_checks.set(seen + 1);
            self.stale_after.is_none_or(|limit| seen < limit)
        }
        fn issue_ticket(&mut self) -> Result<(), String> {
            self.step("ticket")
        }
        fn drain_drafts(&mut self) -> Result<(), String> {
            self.step("drain")
        }
        fn stop_ai(&mut self) -> Result<(), String> {
            self.step("stop_ai")
        }
        fn seal_open_queue(&mut self) -> bool {
            self.calls.push("seal");
            !self.open_waiting_at_seal
        }
        fn unseal_open_queue(&mut self) {
            self.calls.push("unseal");
        }
        fn authorize_exit(&mut self) -> Result<(), String> {
            self.step("authorize")
        }
        fn abort_helper(&mut self) {
            self.calls.push("abort");
        }
        fn resume_ai(&mut self) {
            self.calls.push("resume_ai");
        }
    }

    #[test]
    fn the_exit_is_authorized_last_after_the_ticket_drain_and_ai_stop() {
        let mut steps = FakeExit::default();
        assert_eq!(run_exit_sequence(&mut steps), ExitOutcome::Quit);
        assert_eq!(
            steps.calls,
            ["ticket", "drain", "stop_ai", "seal", "authorize"]
        );
    }

    #[test]
    fn a_request_queued_before_the_seal_gives_up_the_restart_without_authorizing() {
        let mut steps = FakeExit {
            open_waiting_at_seal: true,
            ..FakeExit::default()
        };
        assert!(matches!(run_exit_sequence(&mut steps), ExitOutcome::Kept(_)));
        assert_eq!(
            steps.calls,
            ["ticket", "drain", "stop_ai", "seal", "abort", "resume_ai"]
        );
    }

    #[test]
    fn the_queue_is_sealed_only_when_empty_and_refuses_later_requests() {
        let mut queue = ExternalOpenQueue::new();
        queue.push(PathBuf::from("a.md"));
        assert!(!queue.seal_if_empty(), "a waiting request keeps the queue open");
        assert_eq!(queue.try_push(PathBuf::from("b.md")), Ok(()));
        queue.drain_into(|_| ExternalOpenOutcome::Accepted);
        assert!(queue.seal_if_empty());
        // After the seal (authorization, quit) nothing is queued where this
        // process cannot open it; the sender gets the path back.
        assert_eq!(
            queue.try_push(PathBuf::from("c.md")),
            Err(PathBuf::from("c.md"))
        );
        assert!(queue.is_empty());
        queue.unseal();
        assert_eq!(queue.try_push(PathBuf::from("c.md")), Ok(()));
        assert_eq!(queue.len(), 1);
    }

    #[test]
    fn the_shared_queue_seal_is_atomic_with_the_emptiness_check() {
        let queue = SharedExternalOpenQueue::new();
        let sender = queue.clone();
        // Arrived after the final check but before the seal: kept, restart
        // given up.
        sender.push(PathBuf::from("late.md"));
        assert!(!queue.seal_if_empty());
        queue.drain_into(|_| ExternalOpenOutcome::Accepted);
        assert!(queue.seal_if_empty());
        // Arrived after the seal (authorization / just before quit).
        assert_eq!(
            sender.try_push(PathBuf::from("quit.md")),
            Err(PathBuf::from("quit.md"))
        );
        assert!(queue.is_empty());
    }

    #[test]
    fn a_stale_permit_before_anything_aborts_without_a_ticket() {
        let mut steps = FakeExit {
            stale_after: Some(0),
            ..FakeExit::default()
        };
        assert!(matches!(run_exit_sequence(&mut steps), ExitOutcome::Kept(_)));
        assert_eq!(steps.calls, ["abort"]);
    }

    #[test]
    fn a_permit_that_goes_stale_during_the_ai_stop_aborts_and_restarts_the_service() {
        let mut steps = FakeExit {
            stale_after: Some(1),
            ..FakeExit::default()
        };
        assert!(matches!(run_exit_sequence(&mut steps), ExitOutcome::Kept(_)));
        assert_eq!(steps.calls, ["ticket", "drain", "stop_ai", "abort", "resume_ai"]);
        assert!(!steps.calls.contains(&"authorize"));
    }

    #[test]
    fn a_ticket_or_draft_failure_aborts_before_the_ai_service_is_touched() {
        for failing in ["ticket", "drain"] {
            let mut steps = FakeExit::failing(failing);
            assert!(matches!(run_exit_sequence(&mut steps), ExitOutcome::Kept(_)));
            assert_eq!(steps.calls.last(), Some(&"abort"), "{failing}");
            assert!(!steps.calls.contains(&"stop_ai"), "{failing}");
            assert!(!steps.calls.contains(&"authorize"), "{failing}");
        }
    }

    #[test]
    fn an_ai_stop_timeout_aborts_the_helper_and_brings_the_service_back() {
        let mut steps = FakeExit::failing("stop_ai");
        let outcome = run_exit_sequence(&mut steps);
        assert!(matches!(outcome, ExitOutcome::Kept(ref why) if why.contains("AI")));
        assert_eq!(steps.calls, ["ticket", "drain", "stop_ai", "abort", "resume_ai"]);
    }

    #[test]
    fn a_failed_exit_authorization_keeps_the_window_and_the_ai_service() {
        let mut steps = FakeExit::failing("authorize");
        assert!(matches!(run_exit_sequence(&mut steps), ExitOutcome::Kept(_)));
        assert_eq!(
            steps.calls,
            ["ticket", "drain", "stop_ai", "seal", "authorize", "unseal", "resume_ai"]
        );
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
        diagnostics: Option<String>,
        /// When set, `prepare_restart` answers `Prepared` and logs into it.
        prepared_log: Option<CallLog>,
    }

    impl FakeController {
        fn new(snapshot: AppUpdateSnapshot) -> Arc<Self> {
            Arc::new(Self {
                snapshot: Mutex::new(Some(snapshot)),
                calls: Mutex::new(Vec::new()),
                diagnostics: None,
                prepared_log: None,
            })
        }

        fn with_diagnostics(snapshot: AppUpdateSnapshot, text: &str) -> Arc<Self> {
            Arc::new(Self {
                snapshot: Mutex::new(Some(snapshot)),
                calls: Mutex::new(Vec::new()),
                diagnostics: Some(text.to_owned()),
                prepared_log: None,
            })
        }

        fn with_prepared(snapshot: AppUpdateSnapshot, log: CallLog) -> Arc<Self> {
            Arc::new(Self {
                snapshot: Mutex::new(Some(snapshot)),
                calls: Mutex::new(Vec::new()),
                diagnostics: None,
                prepared_log: Some(log),
            })
        }

        fn prepare_calls(&self) -> usize {
            self.calls()
                .iter()
                .filter(|call| call.starts_with("prepare:"))
                .count()
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
        fn diagnostics_text(&self) -> Option<String> {
            self.diagnostics.clone()
        }
        fn prompt_shown(&self, candidate_id: &str) {
            self.calls
                .lock()
                .unwrap()
                .push(format!("shown:{candidate_id}"));
        }
        fn snooze_prompt(&self, candidate_id: &str) {
            self.calls
                .lock()
                .unwrap()
                .push(format!("snooze:{candidate_id}"));
        }
        fn prepare_restart(
            &self,
            candidate_id: &str,
        ) -> Result<Box<dyn PreparedUpdate>, String> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("prepare:{candidate_id}"));
            match &self.prepared_log {
                Some(log) => Ok(Box::new(LoggingPrepared(log.clone()))),
                None => Err("prepare refused by the test controller".to_owned()),
            }
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

    fn promptable(phase: AppUpdatePhase) -> AppUpdateSnapshot {
        AppUpdateSnapshot {
            restart_supported: true,
            prompt_candidate_id: Some("cand".to_owned()),
            ..snapshot(phase)
        }
    }

    #[test]
    fn prompt_requires_ready_and_restart_support() {
        let model = prompt_model(&promptable(AppUpdatePhase::Ready)).expect("a prompt");
        assert_eq!(model.candidate_id, "cand");
        assert!(model.message.contains("0.27.0"));
        assert!(model.message.contains("再起動して更新しますか"));

        for phase in [
            AppUpdatePhase::Idle,
            AppUpdatePhase::Checking,
            AppUpdatePhase::UpToDate,
            AppUpdatePhase::Available,
            AppUpdatePhase::Downloading,
            AppUpdatePhase::Verifying,
            AppUpdatePhase::Discarding,
            AppUpdatePhase::Failed,
            AppUpdatePhase::Unavailable,
        ] {
            assert_eq!(prompt_model(&promptable(phase)), None, "{phase:?}");
        }
        let unsupported = AppUpdateSnapshot {
            restart_supported: false,
            ..promptable(AppUpdatePhase::Ready)
        };
        assert_eq!(prompt_model(&unsupported), None);
        let restoring = AppUpdateSnapshot {
            restoring: true,
            ..promptable(AppUpdatePhase::Ready)
        };
        assert_eq!(prompt_model(&restoring), None);
        // The service has not allowed a prompt (suppressed, withdrawn, ...).
        let held = AppUpdateSnapshot {
            prompt_candidate_id: None,
            ..promptable(AppUpdatePhase::Ready)
        };
        assert_eq!(prompt_model(&held), None);
        let other = AppUpdateSnapshot {
            prompt_candidate_id: Some("other".to_owned()),
            ..promptable(AppUpdatePhase::Ready)
        };
        assert_eq!(prompt_model(&other), None);
        let no_version = AppUpdateSnapshot {
            candidate_version: None,
            ..promptable(AppUpdatePhase::Ready)
        };
        assert_eq!(prompt_model(&no_version), None);
    }

    #[gpui::test]
    fn stale_prompt_cannot_restart_another_candidate(cx: &mut gpui::TestAppContext) {
        let controller = FakeController::new(promptable(AppUpdatePhase::Ready));
        let (view, cx) = cx.add_window_view(|_, cx| EditorView::new("body\n", "Untitled", cx));
        let attached: Arc<dyn AppUpdateController> = controller.clone();
        view.update(cx, |view, _| view.attach_app_update(Some(attached)));
        view.update(cx, |view, cx| view.start_update_restart(Some("older"), cx));
        let (in_progress, message) =
            view.read_with(cx, |view, _| (view.restart_in_progress, view.restart_message.clone()));
        assert!(!in_progress, "a stale button must not start a restart");
        assert_eq!(message, None);

        view.update(cx, |view, cx| view.start_update_restart(Some("cand"), cx));
        assert!(
            view.read_with(cx, |view, _| view.restart_in_progress),
            "the button of the current candidate goes through the normal restart path"
        );
    }

    #[gpui::test]
    fn saving_the_auto_check_setting_makes_the_next_display_report_again(
        cx: &mut gpui::TestAppContext,
    ) {
        let controller = FakeController::new(promptable(AppUpdatePhase::Ready));
        let (view, cx) = cx.add_window_view(|_, cx| EditorView::new("body\n", "Untitled", cx));
        let attached: Arc<dyn AppUpdateController> = controller.clone();
        view.update(cx, |view, cx| {
            view.attach_app_update(Some(attached));
            cx.notify();
        });
        cx.run_until_parked();
        assert_eq!(controller.calls(), vec!["shown:cand".to_owned()]);

        // Disable and re-enable in the settings screen, then close it: the
        // service forgot the earlier display, so the view reports this one.
        view.update(cx, |view, cx| {
            view.settings_open = true;
            view.set_auto_update_check(false, cx);
            view.set_auto_update_check(true, cx);
            cx.notify();
        });
        cx.run_until_parked();
        view.update(cx, |view, cx| {
            view.settings_open = false;
            cx.notify();
        });
        cx.run_until_parked();
        let shown = controller
            .calls()
            .iter()
            .filter(|call| call.as_str() == "shown:cand")
            .count();
        assert_eq!(shown, 2, "the new display is reported: {:?}", controller.calls());
    }

    #[gpui::test]
    fn prompt_is_not_drawn_or_reported_while_a_tab_close_confirmation_is_open(
        cx: &mut gpui::TestAppContext,
    ) {
        let controller = FakeController::new(promptable(AppUpdatePhase::Ready));
        let (view, cx) = cx.add_window_view(|_, cx| EditorView::new("body\n", "Untitled", cx));
        let attached: Arc<dyn AppUpdateController> = controller.clone();
        view.update(cx, |view, cx| {
            let id = view.sessions.active_id();
            let instance = view.document_instance(id).unwrap();
            view.tab_close_confirm = Some(crate::view::TabCloseConfirm { id, instance });
            view.attach_app_update(Some(attached));
            cx.notify();
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds("app-update-prompt").is_none());
        assert!(
            controller.calls().iter().all(|call| !call.starts_with("shown:")),
            "nothing is recorded as shown behind the dialog: {:?}",
            controller.calls()
        );

        view.update(cx, |view, cx| {
            view.tab_close_confirm = None;
            cx.notify();
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds("app-update-prompt").is_some());
        assert_eq!(controller.calls(), vec!["shown:cand".to_owned()]);
    }

    #[gpui::test]
    fn prompt_does_not_take_editor_focus(cx: &mut gpui::TestAppContext) {
        let controller = FakeController::new(promptable(AppUpdatePhase::Ready));
        let (view, cx) = cx.add_window_view(|_, cx| EditorView::new("body\n", "Untitled", cx));
        let handle = view.read_with(cx, |view, _| view.focus_handle.clone());
        cx.update(|window, app| window.focus(&handle, app));
        let attached: Arc<dyn AppUpdateController> = controller.clone();
        view.update(cx, |view, cx| {
            view.attach_app_update(Some(attached));
            cx.notify();
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds("app-update-prompt").is_some());
        assert!(cx.update(|window, _| handle.is_focused(window)));
        assert_eq!(
            controller.calls(),
            vec!["shown:cand".to_owned()],
            "showing the prompt only records the display, once, and starts no work"
        );
    }

    #[gpui::test]
    fn prompt_buttons_are_keyboard_operable_like_a_click(cx: &mut gpui::TestAppContext) {
        let controller = FakeController::new(promptable(AppUpdatePhase::Ready));
        let (view, cx) = cx.add_window_view(|_, cx| EditorView::new("body\n", "Untitled", cx));
        let attached: Arc<dyn AppUpdateController> = controller.clone();
        view.update(cx, |view, cx| {
            view.attach_app_update(Some(attached));
            cx.notify();
        });
        cx.run_until_parked();
        let (restart, later) = view.read_with(cx, |view, _| {
            (
                view.app_update_restart_focus.clone(),
                view.app_update_later_focus.clone(),
            )
        });
        // Showing the prompt moves no focus.
        assert!(cx.update(|window, _| !restart.is_focused(window) && !later.is_focused(window)));

        // Both keys on "後で" do what the click does.
        cx.update(|window, app| window.focus(&later, app));
        cx.simulate_keystrokes("enter");
        cx.simulate_keystrokes("space");
        assert_eq!(
            controller.calls(),
            vec![
                "shown:cand".to_owned(),
                "snooze:cand".to_owned(),
                "snooze:cand".to_owned()
            ]
        );
        let clicked = cx
            .debug_bounds("app-update-prompt-later")
            .expect("the later button is rendered");
        cx.simulate_click(clicked.center(), gpui::Modifiers::none());
        assert_eq!(
            controller
                .calls()
                .iter()
                .filter(|call| call.as_str() == "snooze:cand")
                .count(),
            3
        );

        // Space on "再起動して更新" takes the normal restart path.
        cx.update(|window, app| window.focus(&restart, app));
        cx.simulate_keystrokes("space");
        // The preparation finishes asynchronously, so assert the recorded call
        // rather than the transient in-progress flag.
        cx.run_until_parked();
        let prepares = controller
            .calls()
            .iter()
            .filter(|call| call.as_str() == "prepare:cand")
            .count();
        assert_eq!(prepares, 1, "Space starts exactly one restart preparation");
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
    fn a_new_card_action_clears_the_diagnostics_copy_notice(cx: &mut gpui::TestAppContext) {
        let controller = FakeController::new(AppUpdateSnapshot::idle("0.26.0"));
        let (view, cx) = cx.add_window_view(|_, cx| EditorView::new("body\n", "Untitled", cx));
        let attached: Arc<dyn AppUpdateController> = controller.clone();
        view.update(cx, |view, _| view.attach_app_update(Some(attached)));
        open_general_settings(&view, cx);
        view.update(cx, |view, cx| view.copy_app_update_diagnostics(cx));
        assert!(
            view.read_with(cx, |view, _| view.restart_message.is_some()),
            "the copy action itself must still report its result"
        );
        view.update(cx, |view, cx| {
            view.run_app_update_action(CardAction::Check, cx);
        });
        assert_eq!(
            view.read_with(cx, |view, _| view.restart_message.clone()),
            None,
            "a new non-restart action must drop the stale copy notice"
        );
    }

    #[gpui::test]
    fn clicking_the_diagnostics_button_copies_the_text_without_starting_work(
        cx: &mut gpui::TestAppContext,
    ) {
        let mut idle = AppUpdateSnapshot::idle("0.26.0");
        idle.diagnostics_available = true;
        let controller = FakeController::with_diagnostics(idle, "diagnostics-fixture\nstage=check");
        let (view, cx) = cx.add_window_view(|_, cx| EditorView::new("body\n", "Untitled", cx));
        let attached: Arc<dyn AppUpdateController> = controller.clone();
        view.update(cx, |view, _| view.attach_app_update(Some(attached)));
        open_general_settings(&view, cx);
        let calls_before = controller.calls();

        let button = cx
            .debug_bounds("settings-app-update-diagnostics")
            .expect("the diagnostics button is rendered when records exist");
        cx.simulate_click(button.center(), gpui::Modifiers::none());
        cx.run_until_parked();

        let copied = cx.read_from_clipboard().and_then(|item| item.text());
        assert_eq!(copied.as_deref(), Some("diagnostics-fixture\nstage=check"));
        assert_eq!(
            view.read_with(cx, |view, _| view.restart_message.clone()),
            Some("更新の診断情報をコピーしました。".to_owned())
        );
        assert_eq!(
            controller.calls(),
            calls_before,
            "copying must not start a check or a download"
        );
        assert!(
            controller
                .calls()
                .iter()
                .all(|call| call != "check" && !call.starts_with("download")),
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
        fn write(&self, _: &Path, _: DraftId, _: &Path, _: &str) -> std::io::Result<()> {
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

    type CallLog = Arc<Mutex<Vec<&'static str>>>;

    struct LoggingPrepared(CallLog);

    impl PreparedUpdate for LoggingPrepared {
        fn issue_ticket(
            &mut self,
            _: &RestartTicketSnapshot,
            _: &dyn Fn() -> bool,
        ) -> Result<(), String> {
            self.0.lock().unwrap().push("ticket");
            Ok(())
        }
        fn authorize_exit(self: Box<Self>) -> Result<(), String> {
            self.0.lock().unwrap().push("authorize");
            Ok(())
        }
        fn abort(self: Box<Self>) {
            self.0.lock().unwrap().push("abort");
        }
    }

    struct LoggingAi(CallLog);

    impl AiLifecycle for LoggingAi {
        fn stop(&self, _: Duration) -> Result<(), String> {
            self.0.lock().unwrap().push("stop_ai");
            Ok(())
        }
        fn resume(&self) -> AiResume {
            self.0.lock().unwrap().push("resume_ai");
            AiResume::Failed
        }
        fn poll_resume(&self) -> AiResume {
            self.0.lock().unwrap().push("poll_resume");
            AiResume::Failed
        }
    }

    /// Stays in `Stopping` (never a handle), and counts the wait's polls.
    struct NeverResumes(Arc<std::sync::atomic::AtomicUsize>);

    impl AiLifecycle for NeverResumes {
        fn stop(&self, _: Duration) -> Result<(), String> {
            Ok(())
        }
        fn resume(&self) -> AiResume {
            AiResume::Stopping
        }
        fn poll_resume(&self) -> AiResume {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            AiResume::Stopping
        }
    }

    /// The pending start failed: counts explicit resumes and polls.
    struct FailsToStart {
        resumes: std::sync::atomic::AtomicUsize,
        polls: std::sync::atomic::AtomicUsize,
        resume_result_is_stopping: bool,
    }

    impl AiLifecycle for FailsToStart {
        fn stop(&self, _: Duration) -> Result<(), String> {
            Ok(())
        }
        fn resume(&self) -> AiResume {
            self.resumes.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if self.resume_result_is_stopping {
                AiResume::Stopping
            } else {
                AiResume::Failed
            }
        }
        fn poll_resume(&self) -> AiResume {
            self.polls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            AiResume::Failed
        }
    }

    #[gpui::test]
    fn a_failed_pending_start_ends_the_wait_without_starting_again(
        cx: &mut gpui::TestAppContext,
    ) {
        let ai = Arc::new(FailsToStart {
            resumes: std::sync::atomic::AtomicUsize::new(0),
            polls: std::sync::atomic::AtomicUsize::new(0),
            resume_result_is_stopping: true,
        });
        let (view, cx) = cx.add_window_view(|_, cx| EditorView::new("", "Untitled", cx));
        let lifecycle: Arc<dyn AiLifecycle> = ai.clone();
        view.update(cx, |_, cx| spawn_ai_resume_wait(lifecycle, cx));
        for _ in 0..50 {
            cx.executor().advance_clock(AI_RESUME_POLL_INTERVAL);
            cx.run_until_parked();
        }
        let order = std::sync::atomic::Ordering::SeqCst;
        assert_eq!(ai.polls.load(order), 1, "the wait ends at the failure");
        assert_eq!(ai.resumes.load(order), 0, "the wait never starts a service");
    }

    #[gpui::test]
    fn an_immediate_start_failure_does_not_begin_a_wait(cx: &mut gpui::TestAppContext) {
        let ai = Arc::new(FailsToStart {
            resumes: std::sync::atomic::AtomicUsize::new(0),
            polls: std::sync::atomic::AtomicUsize::new(0),
            resume_result_is_stopping: false,
        });
        let (view, cx) = cx.add_window_view(|_, cx| EditorView::new("", "Untitled", cx));
        let lifecycle: Arc<dyn AiLifecycle> = ai.clone();
        let observation = match view.update(cx, |view, cx| view.prepare_update_restart(cx)) {
            RestartPreparation::Clear(observation) => observation,
            RestartPreparation::Blocked(_) => panic!("a clean window must be clear"),
        };
        let ticket = view.read_with(cx, |view, _| view.restart_ticket_snapshot());
        view.update(cx, |view, cx| {
            let mut exit = ViewExit {
                view,
                cx,
                prepared: None,
                observation: &observation,
                ticket: &ticket,
                ai: Some(lifecycle),
                ai_stopped: true,
            };
            exit.resume_ai();
        });
        for _ in 0..50 {
            cx.executor().advance_clock(AI_RESUME_POLL_INTERVAL);
            cx.run_until_parked();
        }
        let order = std::sync::atomic::Ordering::SeqCst;
        assert_eq!(ai.resumes.load(order), 1, "one explicit resume");
        assert_eq!(ai.polls.load(order), 0, "no wait after an immediate failure");
    }

    #[gpui::test]
    fn the_ai_resume_wait_keeps_polling_past_the_old_limit(cx: &mut gpui::TestAppContext) {
        let polls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (view, cx) = cx.add_window_view(|_, cx| EditorView::new("", "Untitled", cx));
        let ai: Arc<dyn AiLifecycle> = Arc::new(NeverResumes(polls.clone()));
        view.update(cx, |_, cx| spawn_ai_resume_wait(ai, cx));
        // The old limit was 300 polls (about a minute); the clock is virtual,
        // so going well past it costs no real time.
        for _ in 0..400 {
            cx.executor().advance_clock(AI_RESUME_POLL_INTERVAL);
            cx.run_until_parked();
        }
        assert!(
            polls.load(std::sync::atomic::Ordering::SeqCst) > 300,
            "the wait must outlive the old poll limit"
        );
    }

    /// Runs the post-`Prepared` step after `change` altered the window once
    /// the first check had passed, and returns what the helper/AI saw.
    fn restart_after_change(
        cx: &mut gpui::TestAppContext,
        change: impl FnOnce(&mut EditorView),
    ) -> (Vec<&'static str>, bool, Option<String>) {
        let log: CallLog = Arc::new(Mutex::new(Vec::new()));
        let (view, cx) = cx.add_window_view(|_, cx| EditorView::new("", "Untitled", cx));
        let ai: Arc<dyn AiLifecycle> = Arc::new(LoggingAi(log.clone()));
        view.update(cx, |view, _| view.attach_ai_lifecycle(Some(ai)));
        let observation = match view.update(cx, |view, cx| view.prepare_update_restart(cx)) {
            RestartPreparation::Clear(observation) => observation,
            RestartPreparation::Blocked(_) => panic!("a clean window must be clear"),
        };
        view.update(cx, |view, _| {
            view.restart_in_progress = true;
            change(view);
        });
        let ticket = view.read_with(cx, |view, _| view.restart_ticket_snapshot());
        let prepared: Box<dyn PreparedUpdate> = Box::new(LoggingPrepared(log.clone()));
        view.update(cx, |view, cx| {
            view.continue_update_restart(Ok(prepared), observation, ticket, cx);
        });
        let (frozen, message) = view.read_with(cx, |view, _| {
            assert!(!view.restart_in_progress);
            (view.restart_freeze_active(), view.restart_message.clone())
        });
        let calls = log.lock().unwrap().clone();
        (calls, frozen, message)
    }

    #[gpui::test]
    fn prompts_made_dirty_while_the_helper_prepared_abort_the_restart(
        cx: &mut gpui::TestAppContext,
    ) {
        let (calls, frozen, message) =
            restart_after_change(cx, |view| view.ai_prompts.force_dirty_for_test());
        assert_eq!(calls, ["abort"], "no ticket, AI stop or authorization");
        assert!(!frozen, "the freeze must be released");
        assert!(message.is_some_and(|m| m.contains("AIプロンプト")));
    }

    #[gpui::test]
    fn ai_settings_made_dirty_while_the_helper_prepared_abort_the_restart(
        cx: &mut gpui::TestAppContext,
    ) {
        let (calls, frozen, message) =
            restart_after_change(cx, |view| view.ai_settings.force_dirty_for_test());
        assert_eq!(calls, ["abort"], "no ticket, AI stop or authorization");
        assert!(!frozen, "the freeze must be released");
        assert!(message.is_some_and(|m| m.contains("AI設定")));
    }

    #[gpui::test]
    fn an_ai_operation_started_while_the_helper_prepared_aborts_the_restart(
        cx: &mut gpui::TestAppContext,
    ) {
        let (calls, frozen, message) =
            restart_after_change(cx, |view| view.restart_test_ai_busy = true);
        assert_eq!(calls, ["abort"], "no ticket, AI stop or authorization");
        assert!(!frozen);
        assert!(message.is_some_and(|m| m.contains("AIの処理")));
    }

    #[gpui::test]
    fn an_open_that_started_while_the_helper_prepared_aborts_the_restart(
        cx: &mut gpui::TestAppContext,
    ) {
        let (calls, frozen, _) = restart_after_change(cx, |view| {
            view.loading_paths.insert(PathBuf::from("late.md"));
        });
        assert_eq!(calls, ["abort"]);
        assert!(!frozen);
    }

    /// Stops the AI like [`LoggingAi`], and a Finder document arrives while it
    /// waits (the UI thread is blocked, so nothing can drain the queue).
    struct FinderOpenDuringStop {
        log: CallLog,
        inbox: SharedExternalOpenQueue,
        path: PathBuf,
    }

    impl AiLifecycle for FinderOpenDuringStop {
        fn stop(&self, _: Duration) -> Result<(), String> {
            self.log.lock().unwrap().push("stop_ai");
            self.inbox.push(self.path.clone());
            Ok(())
        }
        fn resume(&self) -> AiResume {
            self.log.lock().unwrap().push("resume_ai");
            AiResume::Failed
        }
        fn poll_resume(&self) -> AiResume {
            AiResume::Failed
        }
    }

    #[gpui::test]
    fn a_finder_open_received_while_the_ai_stops_keeps_the_app_running(
        cx: &mut gpui::TestAppContext,
    ) {
        let path = std::env::temp_dir().join(format!(
            "hane-app-update-finder-{}.md",
            std::process::id()
        ));
        std::fs::write(&path, "double clicked\n").unwrap();
        let log: CallLog = Arc::new(Mutex::new(Vec::new()));
        let inbox = SharedExternalOpenQueue::new();
        let (view, cx) = cx.add_window_view(|_, cx| EditorView::new("", "Untitled", cx));
        let ai: Arc<dyn AiLifecycle> = Arc::new(FinderOpenDuringStop {
            log: log.clone(),
            inbox: inbox.clone(),
            path: path.clone(),
        });
        view.update(cx, |view, _| {
            view.attach_ai_lifecycle(Some(ai));
            view.attach_external_open_inbox(Some(inbox.clone()));
        });
        let observation = match view.update(cx, |view, cx| view.prepare_update_restart(cx)) {
            RestartPreparation::Clear(observation) => observation,
            RestartPreparation::Blocked(_) => panic!("a clean window must be clear"),
        };
        let ticket = view.read_with(cx, |view, _| view.restart_ticket_snapshot());
        let prepared: Box<dyn PreparedUpdate> = Box::new(LoggingPrepared(log.clone()));
        view.update(cx, |view, cx| {
            view.restart_in_progress = true;
            view.continue_update_restart(Ok(prepared), observation, ticket, cx);
        });

        let calls = log.lock().unwrap().clone();
        let restart_message = view.read_with(cx, |view, _| view.restart_message.clone());
        assert!(
            !calls.contains(&"authorize"),
            "the helper must not be released while a document is unhandled: calls={calls:?} restart_message={restart_message:?}"
        );
        assert!(
            calls.contains(&"abort"),
            "abort was not recorded: calls={calls:?} restart_message={restart_message:?}"
        );
        assert!(
            calls.contains(&"resume_ai"),
            "resume_ai was not recorded: calls={calls:?} restart_message={restart_message:?}"
        );
        view.read_with(cx, |view, _| {
            assert!(
                !view.restart_freeze_active(),
                "the user can retry: calls={calls:?} restart_message={restart_message:?}"
            );
            assert!(
                !view.restart_in_progress,
                "restart must not remain in progress: calls={calls:?} restart_message={restart_message:?}"
            );
        });
        assert_eq!(
            inbox.len(),
            1,
            "the document is still waiting, not lost: calls={calls:?} restart_message={restart_message:?}"
        );

        // Once the freeze is gone the waiting document opens in this process.
        let accepted = view.update(cx, |view, cx| {
            inbox.drain_into(|path| view.submit_external_open(path, cx))
        });
        cx.run_until_parked();
        assert_eq!(accepted, 1);
        assert!(inbox.is_empty());
        view.read_with(cx, |view, _| {
            assert_eq!(view.editor().document().full_text(), "double clicked\n");
        });
        std::fs::remove_file(path).unwrap();
    }

    /// Records what the sender of a Finder request sees while the helper is
    /// being authorized (and, afterwards, just before the quit).
    struct FinderOpenDuringAuthorize {
        log: CallLog,
        inbox: SharedExternalOpenQueue,
        refused: Arc<Mutex<Vec<PathBuf>>>,
    }

    impl PreparedUpdate for FinderOpenDuringAuthorize {
        fn issue_ticket(
            &mut self,
            _: &RestartTicketSnapshot,
            _: &dyn Fn() -> bool,
        ) -> Result<(), String> {
            Ok(())
        }
        fn authorize_exit(self: Box<Self>) -> Result<(), String> {
            self.log.lock().unwrap().push("authorize");
            if let Err(path) = self.inbox.try_push(PathBuf::from("during-authorize.md")) {
                self.refused.lock().unwrap().push(path);
            }
            Ok(())
        }
        fn abort(self: Box<Self>) {
            self.log.lock().unwrap().push("abort");
        }
    }

    #[gpui::test]
    fn a_finder_open_during_authorization_is_refused_to_the_sender_not_queued(
        cx: &mut gpui::TestAppContext,
    ) {
        let log: CallLog = Arc::new(Mutex::new(Vec::new()));
        let refused = Arc::new(Mutex::new(Vec::new()));
        let inbox = SharedExternalOpenQueue::new();
        let (view, cx) = cx.add_window_view(|_, cx| EditorView::new("", "Untitled", cx));
        view.update(cx, |view, _| {
            view.attach_external_open_inbox(Some(inbox.clone()));
        });
        let observation = match view.update(cx, |view, cx| view.prepare_update_restart(cx)) {
            RestartPreparation::Clear(observation) => observation,
            RestartPreparation::Blocked(_) => panic!("a clean window must be clear"),
        };
        let ticket = view.read_with(cx, |view, _| view.restart_ticket_snapshot());
        let prepared: Box<dyn PreparedUpdate> = Box::new(FinderOpenDuringAuthorize {
            log: log.clone(),
            inbox: inbox.clone(),
            refused: refused.clone(),
        });
        view.update(cx, |view, cx| {
            view.restart_in_progress = true;
            view.continue_update_restart(Ok(prepared), observation, ticket, cx);
        });
        assert_eq!(log.lock().unwrap().clone(), ["authorize"]);
        assert_eq!(
            refused.lock().unwrap().clone(),
            vec![PathBuf::from("during-authorize.md")],
            "the sender must get the request back to hand to the next process"
        );
        assert!(inbox.is_empty(), "nothing is stranded in the exiting process");
        // Just before the quit (after authorization) the queue stays closed.
        assert!(inbox.try_push(PathBuf::from("before-quit.md")).is_err());
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

    // ---- Issue #745: 保存して再起動 ----

    /// An in-memory filesystem that records every write and can refuse chosen
    /// files. A write only runs once the test lets the executor run, so the
    /// window between "write started" and "write finished" stays open for as
    /// long as the test does not park; nothing here waits on a clock.
    #[derive(Default)]
    struct RecordingFiles {
        inner: hane_session::testing::MemoryFileService,
        saves: Mutex<Vec<PathBuf>>,
        failing: Mutex<HashSet<String>>,
        active: std::sync::atomic::AtomicUsize,
        max_active: std::sync::atomic::AtomicUsize,
    }

    impl RecordingFiles {
        fn saved_names(&self) -> Vec<String> {
            self.saves
                .lock()
                .unwrap()
                .iter()
                .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
                .collect()
        }

        fn fail(&self, name: &str) {
            self.failing.lock().unwrap().insert(name.to_owned());
        }
    }

    impl FileService for RecordingFiles {
        fn load(&self, path: &Path) -> std::io::Result<LoadedFile> {
            self.inner.load(path)
        }
        fn open_reader(&self, path: &Path) -> std::io::Result<hane_session::ReadFile> {
            self.inner.open_reader(path)
        }
        fn save(&self, path: &Path, document: &RopeBuffer) -> std::io::Result<SavedFile> {
            use std::sync::atomic::Ordering;
            let now = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            self.max_active.fetch_max(now, Ordering::SeqCst);
            self.saves.lock().unwrap().push(path.to_path_buf());
            let refused = path
                .file_name()
                .is_some_and(|name| self.failing.lock().unwrap().contains(&*name.to_string_lossy()));
            let result = if refused {
                Err(std::io::Error::other("disk full"))
            } else {
                self.inner.save(path, document)
            };
            self.active.fetch_sub(1, Ordering::SeqCst);
            result
        }
        fn stamp(&self, path: &Path) -> Option<hane_session::FileStamp> {
            self.inner.stamp(path)
        }
        fn rename(&self, from: &Path, to: &Path) -> std::io::Result<()> {
            self.inner.rename(from, to)
        }
        fn rename_folder(&self, from: &Path, to: &Path) -> std::io::Result<()> {
            self.inner.rename_folder(from, to)
        }
        fn create_dir(&self, path: &Path) -> std::io::Result<()> {
            self.inner.create_dir(path)
        }
    }

    struct BatchFixture<'a> {
        view: gpui::Entity<EditorView>,
        cx: &'a mut gpui::VisualTestContext,
        files: Arc<RecordingFiles>,
        controller: Arc<FakeController>,
        ids: Vec<SessionId>,
    }

    /// One file-backed session per name, all clean, with a Ready update.
    fn batch_fixture<'a>(
        cx: &'a mut gpui::TestAppContext,
        names: &[&str],
        controller: Arc<FakeController>,
    ) -> BatchFixture<'a> {
        let files = Arc::new(RecordingFiles::default());
        let paths: Vec<PathBuf> = names
            .iter()
            .map(|name| PathBuf::from(format!("/notes/{name}")))
            .collect();
        for path in &paths {
            files.inner.write_externally(path, "start\n");
        }
        let service: Arc<dyn FileService> = files.clone();
        let build_paths = paths.clone();
        let (view, cx) = cx.add_window_view(move |_, cx| {
            let mut set = SessionSet::with_loaded(service.load(&build_paths[0]).unwrap());
            for path in &build_paths[1..] {
                set.apply_open(None, service.load(path).unwrap());
            }
            EditorView::from_sessions(set, service.clone(), StateStores::memory(), cx)
        });
        let attached: Arc<dyn AppUpdateController> = controller.clone();
        view.update(cx, |view, _| view.attach_app_update(Some(attached)));
        let ids = (0..names.len() as u64).map(SessionId).collect();
        BatchFixture {
            view,
            cx,
            files,
            controller,
            ids,
        }
    }

    impl BatchFixture<'_> {
        fn edit(&mut self, index: usize, text: &str) {
            let id = self.ids[index];
            self.view.update(self.cx, |view, cx| {
                view.sessions.activate(id);
                view.on_document_replaced();
                view.editor_mut().insert_text(text).unwrap();
                view.after_input(cx);
            });
        }

        fn targets(&mut self) -> Option<Vec<SessionId>> {
            self.view.update(self.cx, |view, cx| {
                view.save_and_restart_targets(&promptable(AppUpdatePhase::Ready), cx)
            })
        }

        fn start(&mut self) {
            self.view
                .update(self.cx, |view, cx| view.start_save_and_restart("cand", cx));
        }

        fn park(&mut self) {
            self.cx.run_until_parked();
        }

        fn message(&mut self) -> Option<String> {
            self.view
                .read_with(self.cx, |view, _| view.restart_message.clone())
        }

        fn batch_active(&mut self) -> bool {
            self.view
                .read_with(self.cx, |view, _| view.update_save_batch.is_some())
        }

        fn is_dirty(&mut self, index: usize) -> bool {
            let id = self.ids[index];
            self.view
                .read_with(self.cx, |view, _| view.sessions.get(id).unwrap().is_dirty())
        }

        fn text_of(&mut self, index: usize) -> String {
            let id = self.ids[index];
            self.view.update(self.cx, |view, _| {
                view.sessions.activate(id);
                view.editor().document().full_text()
            })
        }

        fn redraw(&mut self) {
            self.view.update(self.cx, |_, cx| cx.notify());
            self.cx.run_until_parked();
        }
    }

    fn ready_controller() -> Arc<FakeController> {
        FakeController::new(promptable(AppUpdatePhase::Ready))
    }

    #[gpui::test]
    fn save_and_restart_is_offered_only_for_path_backed_dirty_sessions(
        cx: &mut gpui::TestAppContext,
    ) {
        let mut fixture = batch_fixture(cx, &["a.md", "b.md", "c.md"], ready_controller());
        fixture.redraw();
        assert_eq!(fixture.targets(), None, "nothing unsaved: the plain restart");
        assert!(fixture.cx.debug_bounds("app-update-prompt-restart").is_some());
        assert!(
            fixture
                .cx
                .debug_bounds("app-update-prompt-save-restart")
                .is_none()
        );

        // A background (inactive) document is a target too.
        fixture.edit(1, "B");
        fixture.edit(0, "A");
        fixture.redraw();
        assert_eq!(
            fixture.targets(),
            Some(vec![fixture.ids[0], fixture.ids[1]]),
            "ordered by session id, the inactive one included"
        );
        assert!(
            fixture
                .cx
                .debug_bounds("app-update-prompt-save-restart")
                .is_some()
        );
        assert!(fixture.cx.debug_bounds("app-update-prompt-restart").is_none());

        // The check has no side effect: nothing was saved or prepared.
        assert!(fixture.files.saved_names().is_empty());
        assert_eq!(fixture.controller.prepare_calls(), 0);

        // The settings card offers the same action.
        fixture.view.update(fixture.cx, |view, cx| {
            view.settings_open = true;
            view.settings_category = SettingsCategory::General;
            cx.notify();
        });
        fixture.park();
        assert!(fixture.cx.debug_bounds("settings-app-update-card").is_some());
        let card_has_save_action = fixture.view.update(fixture.cx, |view, cx| {
            let snapshot = view.app_update.as_ref().unwrap().snapshot();
            view.save_and_restart_targets(&snapshot, cx).is_some()
        });
        assert!(card_has_save_action);
    }

    #[gpui::test]
    fn a_dirty_unnamed_session_disables_save_and_restart_even_after_draft_flush(
        cx: &mut gpui::TestAppContext,
    ) {
        let mut fixture = batch_fixture(cx, &["a.md"], ready_controller());
        fixture.edit(0, "A");
        assert!(fixture.targets().is_some());
        fixture.view.update(fixture.cx, |view, _| {
            view.sessions.open_untitled("", "Untitled");
            view.on_document_replaced();
            view.editor_mut().insert_text("memo").unwrap();
        });
        assert_eq!(fixture.targets(), None);
        // Writing the draft does not make the note clean.
        let still_dirty = fixture.view.update(fixture.cx, |view, _| {
            let _ = view.flush_pending_drafts_checked();
            view.sessions.active().is_dirty()
        });
        assert!(still_dirty);
        assert_eq!(fixture.targets(), None);
    }

    #[gpui::test]
    fn a_clean_unnamed_session_does_not_disable_save_and_restart(cx: &mut gpui::TestAppContext) {
        let mut fixture = batch_fixture(cx, &["a.md"], ready_controller());
        fixture.view.update(fixture.cx, |view, _| {
            view.sessions.open_untitled("", "Untitled");
        });
        fixture.edit(0, "A");
        assert_eq!(fixture.targets(), Some(vec![fixture.ids[0]]));
    }

    #[gpui::test]
    fn other_restart_blockers_keep_the_existing_message_and_actions(
        cx: &mut gpui::TestAppContext,
    ) {
        let mut fixture = batch_fixture(cx, &["a.md"], ready_controller());
        fixture.edit(0, "A");
        assert!(fixture.targets().is_some());
        fixture.view.update(fixture.cx, |view, _| {
            view.restart_test_ai_busy = true;
            view.save_as_dialogs_open = 1;
            view.loading_paths.insert(PathBuf::from("pending.md"));
        });
        assert_eq!(fixture.targets(), None);
        fixture.redraw();
        assert!(fixture.cx.debug_bounds("app-update-prompt-restart").is_some());
        assert!(
            fixture
                .cx
                .debug_bounds("app-update-prompt-save-restart")
                .is_none()
        );
        fixture.view.update(fixture.cx, |view, cx| {
            view.start_update_restart(Some("cand"), cx);
        });
        let message = fixture.message().unwrap();
        assert!(message.contains("未保存の文書があります"), "{message}");
        assert!(message.contains("AIの処理が実行中"), "{message}");
        assert!(message.contains("保存先の選択"), "{message}");
        assert!(message.contains("ファイルを開いている途中"), "{message}");
        assert!(fixture.files.saved_names().is_empty());

        // Further kinds of blocker are not turned into a save either.
        fixture.view.update(fixture.cx, |view, _| {
            view.restart_test_ai_busy = false;
            view.save_as_dialogs_open = 0;
            view.loading_paths.clear();
            view.file_context_menu_busy = true;
        });
        assert_eq!(fixture.targets(), None);
    }

    #[gpui::test]
    fn save_and_restart_saves_all_targets_before_preparing_the_helper(
        cx: &mut gpui::TestAppContext,
    ) {
        let mut fixture = batch_fixture(cx, &["a.md", "b.md", "c.md"], ready_controller());
        fixture.edit(2, "C");
        fixture.edit(0, "A");
        let (first, second) = (fixture.ids[0], fixture.ids[2]);
        fixture.start();
        assert!(fixture.batch_active());
        // Only the first write is open; the second waits for its completion.
        let in_flight = fixture.view.read_with(fixture.cx, |view, _| {
            (
                view.sessions.get(first).unwrap().save_in_flight(),
                view.sessions.get(second).unwrap().save_in_flight(),
            )
        });
        assert_eq!(in_flight, (true, false));
        assert_eq!(fixture.controller.prepare_calls(), 0);

        fixture.park();
        assert_eq!(fixture.files.saved_names(), ["a.md", "c.md"]);
        assert_eq!(
            fixture.files.max_active.load(std::sync::atomic::Ordering::SeqCst),
            1
        );
        assert!(
            fixture
                .files
                .inner
                .contents("/notes/a.md")
                .is_some_and(|text| text.contains('A'))
        );
        assert!(
            fixture
                .files
                .inner
                .contents("/notes/c.md")
                .is_some_and(|text| text.contains('C'))
        );
        assert_eq!(
            fixture.controller.prepare_calls(),
            1,
            "the helper is prepared once, after every save"
        );
        assert!(!fixture.batch_active());
    }

    #[gpui::test]
    fn save_and_restart_rejoins_the_existing_exit_sequence(cx: &mut gpui::TestAppContext) {
        let log: CallLog = Arc::new(Mutex::new(Vec::new()));
        let controller = FakeController::with_prepared(promptable(AppUpdatePhase::Ready), log.clone());
        let mut fixture = batch_fixture(cx, &["a.md", "b.md"], controller);
        let ai: Arc<dyn AiLifecycle> = Arc::new(LoggingAi(log.clone()));
        fixture
            .view
            .update(fixture.cx, |view, _| view.attach_ai_lifecycle(Some(ai)));
        fixture.edit(0, "A");
        fixture.edit(1, "B");
        fixture.start();
        fixture.park();
        assert_eq!(fixture.files.saved_names(), ["a.md", "b.md"]);
        assert_eq!(
            log.lock().unwrap().clone(),
            ["ticket", "stop_ai", "authorize"],
            "the saved window goes through the ticket, AI stop and authorization"
        );
        assert!(!fixture.is_dirty(0));
        assert!(!fixture.is_dirty(1));
    }

    #[gpui::test]
    fn a_save_failure_stops_the_batch_and_shows_the_target_and_reason(
        cx: &mut gpui::TestAppContext,
    ) {
        let mut fixture = batch_fixture(cx, &["a.md", "b.md", "c.md"], ready_controller());
        fixture.files.fail("b.md");
        fixture.edit(0, "A");
        fixture.edit(1, "B");
        fixture.edit(2, "C");
        fixture.start();
        fixture.park();
        assert_eq!(
            fixture.files.saved_names(),
            ["a.md", "b.md"],
            "the batch stops at the failure"
        );
        let message = fixture.message().unwrap();
        assert!(message.contains("b.md"), "{message}");
        assert!(message.contains("disk full"), "{message}");
        assert!(message.contains("更新を中止"), "{message}");
        assert_eq!(fixture.controller.prepare_calls(), 0);
        assert!(!fixture.batch_active());
        // The finished save stays; the failed and untouched ones keep their text.
        assert!(!fixture.is_dirty(0));
        assert!(fixture.is_dirty(1));
        assert!(fixture.is_dirty(2));
        assert!(fixture.text_of(1).contains('B'));

        // Both surfaces show it.
        fixture.redraw();
        assert!(fixture.cx.debug_bounds("app-update-prompt-message").is_some());
        fixture.view.update(fixture.cx, |view, cx| {
            view.settings_open = true;
            view.settings_category = SettingsCategory::General;
            cx.notify();
        });
        fixture.park();
        assert!(
            fixture
                .cx
                .debug_bounds("settings-app-update-restart-message")
                .is_some()
        );
    }

    #[gpui::test]
    fn an_external_conflict_never_forces_save_or_restart(cx: &mut gpui::TestAppContext) {
        let mut fixture = batch_fixture(cx, &["a.md", "b.md"], ready_controller());
        fixture.edit(0, "A");
        fixture.edit(1, "B");
        fixture
            .files
            .inner
            .write_externally("/notes/a.md", "external\n");
        fixture.start();
        fixture.park();
        assert_eq!(
            fixture.files.inner.contents("/notes/a.md").as_deref(),
            Some("external\n"),
            "the external version is never overwritten"
        );
        assert!(fixture.files.saved_names().is_empty());
        let message = fixture.message().unwrap();
        assert!(message.contains("a.md"), "{message}");
        assert!(message.contains("外部"), "{message}");
        assert_eq!(fixture.controller.prepare_calls(), 0);
        assert!(fixture.is_dirty(0));
        assert!(fixture.is_dirty(1));
    }

    #[gpui::test]
    fn a_stale_save_aborts_restart_without_losing_new_edits(cx: &mut gpui::TestAppContext) {
        let mut fixture = batch_fixture(cx, &["a.md"], ready_controller());
        fixture.edit(0, "A");
        fixture.start();
        // The write is open; the document is edited before it finishes.
        fixture.edit(0, "Z");
        fixture.park();
        assert_eq!(fixture.files.saved_names(), ["a.md"]);
        assert_eq!(fixture.controller.prepare_calls(), 0);
        assert!(fixture.is_dirty(0), "the newer edit is still unsaved");
        let text = fixture.text_of(0);
        assert!(text.contains('Z') && text.contains('A'), "{text}");
        assert!(fixture.message().unwrap().contains("更新を中止"));
    }

    #[gpui::test]
    fn changes_to_another_session_abort_the_batch(cx: &mut gpui::TestAppContext) {
        let mut fixture = batch_fixture(cx, &["a.md", "b.md", "c.md"], ready_controller());
        fixture.edit(0, "A");
        fixture.edit(1, "B");
        fixture.start();
        // A document outside the batch is edited while the first write runs.
        fixture.edit(2, "C");
        fixture.park();
        assert_eq!(
            fixture.files.saved_names(),
            ["a.md"],
            "the second target is never started"
        );
        let message = fixture.message().unwrap();
        assert!(message.contains("c.md"), "{message}");
        assert_eq!(fixture.controller.prepare_calls(), 0);
        assert!(fixture.is_dirty(1));
    }

    #[gpui::test]
    fn replaced_sessions_and_changed_paths_cannot_continue_the_batch(
        cx: &mut gpui::TestAppContext,
    ) {
        let mut fixture = batch_fixture(cx, &["a.md"], ready_controller());
        fixture.edit(0, "A");
        let id = fixture.ids[0];
        let problem = |fixture: &mut BatchFixture<'_>, change: &dyn Fn(&mut UpdateSaveBatch)| {
            fixture.view.update(fixture.cx, |view, cx| {
                let mut batch = UpdateSaveBatch {
                    candidate_id: "cand".to_owned(),
                    workspace_generation: view.work_folder_generation,
                    sessions: view.update_save_sessions(),
                    targets: vec![UpdateSaveTarget {
                        id,
                        label: view.sessions.get(id).unwrap().label(),
                        path: view.sessions.get(id).unwrap().path().unwrap().to_path_buf(),
                    }],
                    next: 0,
                    awaiting: None,
                };
                change(&mut batch);
                view.update_save_problem(&batch, false, cx)
            })
        };
        assert_eq!(problem(&mut fixture, &|_| {}), None);
        let swapped = problem(&mut fixture, &|batch| {
            batch.sessions[0].instance = None;
        });
        assert!(swapped.is_some_and(|text| text.contains("置き換わった")));
        let moved = problem(&mut fixture, &|batch| {
            batch.sessions[0].path = Some(PathBuf::from("/elsewhere/a.md"));
        });
        assert!(moved.is_some_and(|text| text.contains("保存先が変更")));
        let workspace = problem(&mut fixture, &|batch| batch.workspace_generation += 1);
        assert!(workspace.is_some_and(|text| text.contains("作業フォルダー")));
        let candidate = problem(&mut fixture, &|batch| {
            batch.candidate_id = "other".to_owned();
        });
        assert!(candidate.is_some_and(|text| text.contains("候補")));

        // A session added or closed after the start changes the composition.
        fixture.view.update(fixture.cx, |view, _| {
            view.sessions.open_untitled("", "Untitled");
        });
        let composition = problem(&mut fixture, &|batch| {
            batch.sessions.pop();
        });
        assert!(composition.is_some_and(|text| text.contains("構成")));
    }

    #[gpui::test]
    fn double_activation_starts_only_one_save_batch(cx: &mut gpui::TestAppContext) {
        let mut fixture = batch_fixture(cx, &["a.md"], ready_controller());
        fixture.edit(0, "A");
        fixture.view.update(fixture.cx, |view, cx| {
            view.start_save_and_restart("cand", cx);
            // Mouse, keyboard, the card and the plain restart entrance again.
            view.start_save_and_restart("cand", cx);
            view.run_app_update_action(CardAction::SaveAndRestart("cand".to_owned()), cx);
            view.run_app_update_action(CardAction::Restart, cx);
            view.start_update_restart(Some("cand"), cx);
        });
        fixture.park();
        assert_eq!(fixture.files.saved_names(), ["a.md"]);
        assert_eq!(fixture.controller.prepare_calls(), 1);
    }

    #[gpui::test]
    fn a_changed_candidate_cannot_be_restarted_after_saving(cx: &mut gpui::TestAppContext) {
        let mut fixture = batch_fixture(cx, &["a.md"], ready_controller());
        fixture.edit(0, "A");
        // A button drawn for an older candidate does nothing.
        fixture.view.update(fixture.cx, |view, cx| {
            view.start_save_and_restart("older", cx);
        });
        assert!(!fixture.batch_active());
        assert!(fixture.files.saved_names().is_empty());

        fixture.start();
        *fixture.controller.snapshot.lock().unwrap() = Some(AppUpdateSnapshot {
            candidate_id: Some("other".to_owned()),
            ..promptable(AppUpdatePhase::Ready)
        });
        fixture.park();
        assert_eq!(fixture.controller.prepare_calls(), 0);
        assert!(fixture.message().unwrap().contains("候補"));
        assert!(!fixture.batch_active());
    }

    #[gpui::test]
    fn tracked_save_reports_every_terminal_path_once(cx: &mut gpui::TestAppContext) {
        let outcomes = [
            (SaveOutcome::Saved, TrackedSaveResult::Saved),
            (SaveOutcome::SavedStale, TrackedSaveResult::SavedStale),
            (SaveOutcome::Conflict, TrackedSaveResult::Conflict),
            (SaveOutcome::Superseded, TrackedSaveResult::Superseded),
            (
                SaveOutcome::Failed(std::io::Error::other("boom")),
                TrackedSaveResult::Failed("boom".to_owned()),
            ),
        ];
        for (outcome, expected) in outcomes {
            assert_eq!(TrackedSaveResult::from_outcome(&outcome), expected);
        }

        let mut fixture = batch_fixture(cx, &["a.md"], ready_controller());
        fixture.edit(0, "A");
        let id = fixture.ids[0];
        let (gone, started, queued) = fixture.view.update(fixture.cx, |view, cx| {
            let gone = view.save_session_tracked(SessionId(99), SaveIntent::Current, cx);
            let started = view.save_session_tracked(id, SaveIntent::Current, cx);
            let queued = view.save_session_tracked(id, SaveIntent::Current, cx);
            (gone, started, queued)
        });
        assert_eq!(gone, SaveStart::Gone);
        assert!(matches!(started, SaveStart::Started(_)));
        assert_eq!(queued, SaveStart::Queued);
        fixture.park();

        // An untitled document has no path to write.
        let needs_path = fixture.view.update(fixture.cx, |view, cx| {
            let untitled = view.sessions.open_untitled("", "Untitled");
            view.save_session_tracked(untitled, SaveIntent::Current, cx)
        });
        assert_eq!(needs_path, SaveStart::NeedsPath);
    }

    #[gpui::test]
    fn a_document_replaced_during_the_write_is_reported_and_stops_the_batch(
        cx: &mut gpui::TestAppContext,
    ) {
        let mut fixture = batch_fixture(cx, &["a.md"], ready_controller());
        fixture.edit(0, "A");
        fixture.start();
        // The workspace is exchanged while the write is open: the completion no
        // longer belongs to this document, and the batch is told so.
        fixture
            .view
            .update(fixture.cx, |view, _| view.work_folder_generation += 1);
        fixture.park();
        assert!(!fixture.batch_active());
        assert_eq!(fixture.controller.prepare_calls(), 0);
        assert!(fixture.message().unwrap().contains("更新を中止"));
    }

    #[gpui::test]
    fn post_save_pending_work_blocks_restart(cx: &mut gpui::TestAppContext) {
        let mut fixture = batch_fixture(cx, &["a.md"], ready_controller());
        fixture.edit(0, "A");
        fixture.start();
        // Something else starts while the write is open and is still going on
        // when the write has been reported.
        fixture.view.update(fixture.cx, |view, _| {
            view.loading_paths.insert(PathBuf::from("late.md"));
        });
        fixture.park();
        assert_eq!(fixture.files.saved_names(), ["a.md"]);
        assert_eq!(fixture.controller.prepare_calls(), 0);
        let message = fixture.message().unwrap();
        assert!(message.contains("ファイルを開いている途中"), "{message}");
    }

    #[gpui::test]
    fn persistence_failure_after_batch_save_still_blocks_restart(cx: &mut gpui::TestAppContext) {
        let root = draft_root("save-restart-draft");
        std::fs::create_dir_all(&root).unwrap();
        let work_folder = OsWorkFolderScanner.scan(&root).unwrap();
        let mut fixture = batch_fixture(cx, &["a.md"], ready_controller());
        fixture.view.update(fixture.cx, |view, cx| {
            view.work_folder = Some(work_folder);
            view.new_work_folder_note(cx);
            view.draft_store = Arc::new(FailingDraftStore);
        });
        fixture.edit(0, "A");
        fixture.start();
        fixture.park();
        assert_eq!(fixture.files.saved_names(), ["a.md"]);
        assert_eq!(
            fixture.controller.prepare_calls(),
            0,
            "a draft that cannot be written still stops the restart"
        );
        let message = fixture.message().unwrap();
        assert!(message.contains("下書きを書き込めませんでした"), "{message}");
        std::fs::remove_dir_all(&root).unwrap();
    }
}
