//! Application-owned, GPUI-independent account/model service.
//!
//! All App Server calls happen on this worker. The UI receives sanitized
//! snapshots and can close/recreate its view without owning the runtime.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::account::{
    AccountState, CancelStatus, account_read_params, completed_login_matches, parse_account_read,
    parse_cancel_status, parse_chatgpt_login_start,
};
use crate::connect::{
    ConnectError, ExpectedCredentialState, SaveOutcome, build_runtime_config_for_active_connection,
    delete_custom_credential_detailed, recover_at_startup, update_custom_credential_detailed,
};
use crate::credential_journal::CredentialJournal;
use crate::owner_lock::{OwnerLock, OwnerLockGuard};
use crate::paths::AiPaths;
use crate::provider::ShellEnvironmentPolicyFormat;
use crate::rpc::RejectAllServerRequests;
use crate::runtime::{AiRuntime, RuntimeEvent, RuntimeEventKind, RuntimeState};
use crate::secrets::{CredentialRef, CredentialStore};
use crate::settings::{ActiveConnection, AiSettings, AiSettingsStore, SaveError};

const COMMAND_CAPACITY: usize = 16;
const SNAPSHOT_SUBSCRIBER_CAPACITY: usize = 8;
const RPC_TIMEOUT: Duration = Duration::from_secs(20);
const LOGIN_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const EVENT_POLL: Duration = Duration::from_millis(40);
const MAX_EARLY_LOGIN_EVENTS: usize = 64;
const MAX_EARLY_LOGIN_BYTES: usize = 256 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct OperationId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceBusyReason {
    Saving,
    Recovering,
    Login,
    Account,
    Models,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OwnershipState {
    Unknown,
    Owned,
    OwnedElsewhere,
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoginState {
    Idle,
    Starting,
    AwaitingBrowser,
    Reconciling,
    Canceling,
    Finished,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelListState {
    NotLoaded,
    Loading,
    Loaded(Vec<crate::models::ChatGptModel>),
    Failed(crate::models::ModelListError),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SafeOperationResult {
    Succeeded,
    Failed(&'static str),
    Canceled,
    TimedOut,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PersistenceState {
    Clean,
    NotCommitted,
    Saved,
    CleanupPending,
    DurabilityUnconfirmed,
    RecoveryRequired,
}

#[derive(Debug, Clone)]
pub struct AiSnapshot {
    pub state_version: u64,
    pub ownership: OwnershipState,
    pub settings: AiSettings,
    pub runtime_state: RuntimeState,
    pub runtime_generation: u64,
    pub configured_settings_generation: u64,
    pub account: AccountState,
    pub account_refresh_failed: bool,
    pub login: LoginState,
    pub model_list: ModelListState,
    pub auth_epoch: u64,
    pub busy: Option<(OperationId, ServiceBusyReason)>,
    pub last_result: Option<(OperationId, SafeOperationResult)>,
    pub recovery_required: bool,
    pub persistence: PersistenceState,
}

impl Default for AiSnapshot {
    fn default() -> Self {
        Self {
            state_version: 0,
            ownership: OwnershipState::Unknown,
            settings: AiSettings::default(),
            runtime_state: RuntimeState::Stopped,
            runtime_generation: 0,
            configured_settings_generation: 0,
            account: AccountState::Unknown,
            account_refresh_failed: false,
            login: LoginState::Idle,
            model_list: ModelListState::NotLoaded,
            auth_epoch: 0,
            busy: None,
            last_result: None,
            recovery_required: false,
            persistence: PersistenceState::Clean,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdmissionError {
    Busy,
    QueueFull,
    Unavailable,
    WrongOperation,
}

#[derive(Debug, Clone, Copy)]
pub enum AiCommand {
    OpenSettings,
    RefreshAccount { refresh_token: bool },
    StartLogin,
    Logout,
    RefreshModels,
}

#[derive(Clone)]
pub struct AiServiceHandle {
    command_tx: SyncSender<QueuedCommand>,
    cancel_tx: SyncSender<OperationId>,
    shared: Arc<SharedServiceState>,
}

struct SharedServiceState {
    snapshot: Mutex<AiSnapshot>,
    busy: Mutex<Option<(OperationId, ServiceBusyReason)>>,
    next_operation_id: AtomicU64,
    subscribers: Mutex<Vec<SyncSender<AiSnapshot>>>,
    available: Mutex<bool>,
}

struct QueuedCommand {
    id: OperationId,
    command: WorkerCommand,
}

enum WorkerCommand {
    Action(AiCommand),
    SaveSettings {
        expected_revision: u64,
        settings: AiSettings,
    },
    UpdateCustomCredential {
        expected_revision: u64,
        old_credential_ref: Option<CredentialRef>,
        settings_without_credential: AiSettings,
        secret: String,
    },
    DeleteCustomCredential {
        expected_revision: u64,
        old_credential_ref: CredentialRef,
        settings_without_credential: AiSettings,
    },
}

pub struct AiService {
    handle: AiServiceHandle,
    shutdown_tx: mpsc::Sender<()>,
    worker: Option<JoinHandle<()>>,
}

impl AiServiceHandle {
    pub fn snapshot(&self) -> AiSnapshot {
        self.shared.snapshot.lock().unwrap().clone()
    }

    pub fn subscribe(&self) -> Receiver<AiSnapshot> {
        let (tx, rx) = mpsc::sync_channel(SNAPSHOT_SUBSCRIBER_CAPACITY);
        let mut subscribers = self.shared.subscribers.lock().unwrap();
        let _ = tx.try_send(self.snapshot());
        subscribers.push(tx);
        rx
    }

    pub fn try_submit(&self, command: AiCommand) -> Result<OperationId, AdmissionError> {
        let reason = match command {
            AiCommand::OpenSettings => ServiceBusyReason::Recovering,
            AiCommand::RefreshAccount { .. } | AiCommand::Logout => ServiceBusyReason::Account,
            AiCommand::StartLogin => ServiceBusyReason::Login,
            AiCommand::RefreshModels => ServiceBusyReason::Models,
        };
        self.enqueue(WorkerCommand::Action(command), reason)
    }

    pub fn try_save_settings(
        &self,
        expected_revision: u64,
        settings: AiSettings,
    ) -> Result<OperationId, AdmissionError> {
        self.enqueue(
            WorkerCommand::SaveSettings {
                expected_revision,
                settings,
            },
            ServiceBusyReason::Saving,
        )
    }

    pub fn try_update_custom_credential(
        &self,
        expected_revision: u64,
        old_credential_ref: Option<CredentialRef>,
        settings_without_credential: AiSettings,
        secret: String,
    ) -> Result<OperationId, AdmissionError> {
        self.enqueue(
            WorkerCommand::UpdateCustomCredential {
                expected_revision,
                old_credential_ref,
                settings_without_credential,
                secret,
            },
            ServiceBusyReason::Saving,
        )
    }

    pub fn try_delete_custom_credential(
        &self,
        expected_revision: u64,
        old_credential_ref: CredentialRef,
        settings_without_credential: AiSettings,
    ) -> Result<OperationId, AdmissionError> {
        self.enqueue(
            WorkerCommand::DeleteCustomCredential {
                expected_revision,
                old_credential_ref,
                settings_without_credential,
            },
            ServiceBusyReason::Saving,
        )
    }

    fn enqueue(
        &self,
        command: WorkerCommand,
        reason: ServiceBusyReason,
    ) -> Result<OperationId, AdmissionError> {
        if !*self.shared.available.lock().unwrap() {
            return Err(AdmissionError::Unavailable);
        }
        let mut busy = self.shared.busy.lock().unwrap();
        if busy.is_some() {
            return Err(AdmissionError::Busy);
        }
        let id = OperationId(
            self.shared
                .next_operation_id
                .fetch_add(1, Ordering::Relaxed),
        );
        *busy = Some((id, reason));
        mutate(&self.shared, |snapshot| snapshot.busy = Some((id, reason)));
        drop(busy);
        match self.command_tx.try_send(QueuedCommand { id, command }) {
            Ok(()) => Ok(id),
            Err(TrySendError::Full(_)) => {
                *self.shared.busy.lock().unwrap() = None;
                mutate(&self.shared, |snapshot| snapshot.busy = None);
                Err(AdmissionError::QueueFull)
            }
            Err(TrySendError::Disconnected(_)) => {
                *self.shared.busy.lock().unwrap() = None;
                mutate(&self.shared, |snapshot| snapshot.busy = None);
                Err(AdmissionError::Unavailable)
            }
        }
    }

    /// Cancellation has its own bounded channel and bypasses the ordinary
    /// operation admission gate.
    pub fn cancel(&self, target: OperationId) -> Result<(), AdmissionError> {
        let busy = *self.shared.busy.lock().unwrap();
        if busy != Some((target, ServiceBusyReason::Login)) {
            return Err(AdmissionError::WrongOperation);
        }
        self.cancel_tx
            .try_send(target)
            .map_err(|error| match error {
                TrySendError::Full(_) => AdmissionError::QueueFull,
                TrySendError::Disconnected(_) => AdmissionError::Unavailable,
            })
    }
}

pub trait BrowserOpener: Send + Sync + 'static {
    fn open(&self, url: &str) -> Result<(), BrowserOpenError>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BrowserOpenError;

impl std::fmt::Display for BrowserOpenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("The sign-in page could not be opened in the browser.")
    }
}

impl std::error::Error for BrowserOpenError {}

#[derive(Debug, Default)]
pub struct SystemBrowserOpener;

impl BrowserOpener for SystemBrowserOpener {
    fn open(&self, url: &str) -> Result<(), BrowserOpenError> {
        #[cfg(target_os = "macos")]
        let mut command = {
            let mut command = Command::new("open");
            command.arg(url);
            command
        };
        #[cfg(target_os = "windows")]
        let mut command = {
            let mut command = Command::new("rundll32.exe");
            command.args(["url.dll,FileProtocolHandler", url]);
            command
        };
        #[cfg(all(unix, not(target_os = "macos")))]
        let mut command = {
            let mut command = Command::new("xdg-open");
            command.arg(url);
            command
        };
        command.spawn().map(|_| ()).map_err(|_| BrowserOpenError)
    }
}

pub struct AiServiceConfig {
    pub app_data_root: PathBuf,
    pub binary_path: PathBuf,
    pub credential_store: Arc<dyn CredentialStore>,
    pub browser_opener: Arc<dyn BrowserOpener>,
    pub shell_env_format: ShellEnvironmentPolicyFormat,
}

impl AiService {
    pub fn spawn(config: AiServiceConfig) -> std::io::Result<AiService> {
        let (command_tx, command_rx) = mpsc::sync_channel(COMMAND_CAPACITY);
        let (cancel_tx, cancel_rx) = mpsc::sync_channel(4);
        let (shutdown_tx, shutdown_rx) = mpsc::channel();
        let shared = Arc::new(SharedServiceState {
            snapshot: Mutex::new(AiSnapshot::default()),
            busy: Mutex::new(None),
            next_operation_id: AtomicU64::new(1),
            subscribers: Mutex::new(Vec::new()),
            available: Mutex::new(true),
        });
        let handle = AiServiceHandle {
            command_tx,
            cancel_tx,
            shared: shared.clone(),
        };
        let worker = thread::Builder::new()
            .name("hane-ai-service".to_string())
            .spawn(move || service_worker(config, command_rx, cancel_rx, shutdown_rx, shared))?;
        Ok(AiService {
            handle,
            shutdown_tx,
            worker: Some(worker),
        })
    }

    pub fn handle(&self) -> AiServiceHandle {
        self.handle.clone()
    }
}

impl Drop for AiService {
    fn drop(&mut self) {
        *self.handle.shared.available.lock().unwrap() = false;
        let _ = self.shutdown_tx.send(());
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

struct LoginAttempt {
    id: OperationId,
    login_id: String,
    runtime_generation: u64,
    settings_generation: u64,
    deadline: Instant,
}

struct WorkerState {
    runtime: Option<AiRuntime>,
    runtime_events: Option<Receiver<RuntimeEvent>>,
    owner_guard: Option<OwnerLockGuard>,
    attempt: Option<LoginAttempt>,
    early_login_events: VecDeque<(u64, Option<Value>, usize)>,
    early_login_bytes: usize,
}

fn service_worker(
    config: AiServiceConfig,
    command_rx: Receiver<QueuedCommand>,
    cancel_rx: Receiver<OperationId>,
    shutdown_rx: Receiver<()>,
    shared: Arc<SharedServiceState>,
) {
    let paths = AiPaths::new(&config.app_data_root);
    let settings_store = AiSettingsStore::new(
        paths.settings_path(),
        paths.settings_lock_path(),
        paths.runtime_owner_lock_path(),
    );
    let journal = CredentialJournal::new(paths.credential_journal_path());
    let mut state = WorkerState {
        runtime: None,
        runtime_events: None,
        owner_guard: None,
        attempt: None,
        early_login_events: VecDeque::new(),
        early_login_bytes: 0,
    };
    let mut settings = AiSettings::default();
    let mut shutdown = false;

    while !shutdown {
        if shutdown_rx.try_recv().is_ok() {
            break;
        }
        drain_runtime_events(
            &config,
            &paths,
            &settings_store,
            &settings,
            &shared,
            &mut state,
        );
        if let Ok(cancel_id) = cancel_rx.try_recv() {
            handle_cancel(
                cancel_id,
                &config,
                &paths,
                &settings_store,
                &settings,
                &shared,
                &mut state,
                SafeOperationResult::Canceled,
            );
        }
        if let Some(attempt) = state.attempt.as_ref()
            && Instant::now() >= attempt.deadline
        {
            let id = attempt.id;
            handle_timeout(
                id,
                &config,
                &paths,
                &settings_store,
                &settings,
                &shared,
                &mut state,
            );
        }

        match command_rx.recv_timeout(EVENT_POLL) {
            Ok(queued) => handle_command(
                queued,
                &config,
                &paths,
                &settings_store,
                &journal,
                &shared,
                &mut state,
                &mut settings,
            ),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => shutdown = true,
        }
    }

    if let Some(runtime) = state.runtime.take() {
        let _ = runtime.stop();
    }
    *shared.available.lock().unwrap() = false;
}

#[allow(clippy::too_many_arguments)] // The bounded worker owns these references for one command dispatch.
fn handle_command(
    queued: QueuedCommand,
    config: &AiServiceConfig,
    paths: &AiPaths,
    settings_store: &AiSettingsStore,
    journal: &CredentialJournal,
    shared: &Arc<SharedServiceState>,
    state: &mut WorkerState,
    settings: &mut AiSettings,
) {
    if state.attempt.is_some()
        && !matches!(
            &queued.command,
            WorkerCommand::Action(AiCommand::StartLogin)
        )
    {
        finish(
            shared,
            queued.id,
            SafeOperationResult::Failed("login_in_progress"),
            LoginState::AwaitingBrowser,
        );
        return;
    }
    match queued.command {
        WorkerCommand::Action(AiCommand::OpenSettings) => {
            publish_busy(shared, queued.id, ServiceBusyReason::Recovering);
            match open_settings(
                paths,
                settings_store,
                journal,
                config.credential_store.clone(),
                state,
            ) {
                Ok((current, recovery_pending)) => {
                    *settings = current.clone();
                    mutate(shared, |snapshot| {
                        snapshot.settings = current;
                        snapshot.ownership = OwnershipState::Owned;
                        let durability_unconfirmed =
                            snapshot.persistence == PersistenceState::DurabilityUnconfirmed;
                        snapshot.recovery_required = recovery_pending || durability_unconfirmed;
                        snapshot.persistence = if recovery_pending {
                            PersistenceState::RecoveryRequired
                        } else if durability_unconfirmed {
                            PersistenceState::DurabilityUnconfirmed
                        } else {
                            PersistenceState::Clean
                        };
                    });
                    finish(
                        shared,
                        queued.id,
                        SafeOperationResult::Succeeded,
                        LoginState::Idle,
                    );
                }
                Err(OwnerOpenError::Elsewhere) => {
                    mutate(shared, |snapshot| {
                        snapshot.ownership = OwnershipState::OwnedElsewhere
                    });
                    finish(
                        shared,
                        queued.id,
                        SafeOperationResult::Failed("owned_elsewhere"),
                        LoginState::Idle,
                    );
                }
                Err(OwnerOpenError::Failed) => {
                    mutate(shared, |snapshot| {
                        snapshot.ownership = OwnershipState::Unavailable
                    });
                    finish(
                        shared,
                        queued.id,
                        SafeOperationResult::Failed("recovery_failed"),
                        LoginState::Idle,
                    );
                }
            }
        }
        WorkerCommand::Action(AiCommand::RefreshAccount { refresh_token }) => {
            if !settings_actions_allowed(shared) {
                finish(
                    shared,
                    queued.id,
                    SafeOperationResult::Failed("recovery_required"),
                    LoginState::Idle,
                );
                return;
            }
            if !chatgpt_active(settings) {
                finish(
                    shared,
                    queued.id,
                    SafeOperationResult::Failed("chatgpt_not_active"),
                    LoginState::Idle,
                );
                return;
            }
            publish_busy(shared, queued.id, ServiceBusyReason::Account);
            let result = ensure_runtime(config, paths, settings, state)
                .and_then(|runtime| call_account_read(runtime, refresh_token));
            match result {
                Ok(account) => {
                    let signed_in = matches!(account, AccountState::SignedIn { .. });
                    mutate(shared, |snapshot| {
                        if snapshot.account != account {
                            snapshot.auth_epoch += 1;
                        }
                        snapshot.account = account;
                        snapshot.account_refresh_failed = false;
                        if signed_in {
                            snapshot.model_list = ModelListState::NotLoaded;
                        }
                    });
                    finish(
                        shared,
                        queued.id,
                        SafeOperationResult::Succeeded,
                        LoginState::Idle,
                    );
                }
                Err(_) => {
                    mutate(shared, |snapshot| snapshot.account_refresh_failed = true);
                    finish(
                        shared,
                        queued.id,
                        SafeOperationResult::Failed("account_unknown"),
                        LoginState::Idle,
                    );
                }
            }
        }
        WorkerCommand::Action(AiCommand::StartLogin) => {
            start_login(queued.id, config, paths, settings, shared, state)
        }
        WorkerCommand::Action(AiCommand::Logout) => {
            if !settings_actions_allowed(shared) {
                finish(
                    shared,
                    queued.id,
                    SafeOperationResult::Failed("recovery_required"),
                    LoginState::Idle,
                );
                return;
            }
            if !chatgpt_active(settings) {
                finish(
                    shared,
                    queued.id,
                    SafeOperationResult::Failed("chatgpt_not_active"),
                    LoginState::Idle,
                );
                return;
            }
            publish_busy(shared, queued.id, ServiceBusyReason::Account);
            let result = ensure_runtime(config, paths, settings, state).and_then(|runtime| {
                runtime
                    .call("account/logout", None, RPC_TIMEOUT)
                    .map_err(|_| ())?;
                call_account_read(runtime, false)
            });
            match result {
                Ok(account) => {
                    let signed_out = matches!(account, AccountState::SignedOut);
                    mutate(shared, |snapshot| {
                        snapshot.account = account;
                        snapshot.auth_epoch += 1;
                        snapshot.account_refresh_failed = false;
                        snapshot.model_list = ModelListState::NotLoaded;
                    });
                    finish(
                        shared,
                        queued.id,
                        if signed_out {
                            SafeOperationResult::Succeeded
                        } else {
                            SafeOperationResult::Failed("account_unknown")
                        },
                        LoginState::Idle,
                    );
                }
                Err(_) => finish(
                    shared,
                    queued.id,
                    SafeOperationResult::Failed("logout_failed"),
                    LoginState::Idle,
                ),
            }
        }
        WorkerCommand::Action(AiCommand::RefreshModels) => {
            if !settings_actions_allowed(shared) {
                finish(
                    shared,
                    queued.id,
                    SafeOperationResult::Failed("recovery_required"),
                    LoginState::Idle,
                );
                return;
            }
            if !chatgpt_active(settings) {
                finish(
                    shared,
                    queued.id,
                    SafeOperationResult::Failed("chatgpt_not_active"),
                    LoginState::Idle,
                );
                return;
            }
            publish_busy(shared, queued.id, ServiceBusyReason::Models);
            mutate(shared, |snapshot| {
                snapshot.model_list = ModelListState::Loading
            });
            let result = match ensure_runtime(config, paths, settings, state) {
                Ok(runtime) => crate::models::fetch_chatgpt_models(|method, params| {
                    runtime
                        .call(method, Some(params), RPC_TIMEOUT)
                        .map_err(|_| ())
                }),
                Err(()) => Err(crate::models::ModelListError::Rpc),
            };
            match result {
                Ok(models) => {
                    mutate(shared, |snapshot| {
                        snapshot.model_list = ModelListState::Loaded(models)
                    });
                    finish(
                        shared,
                        queued.id,
                        SafeOperationResult::Succeeded,
                        LoginState::Idle,
                    );
                }
                Err(crate::models::ModelListError::Rpc) => {
                    mutate(shared, |snapshot| {
                        snapshot.model_list =
                            ModelListState::Failed(crate::models::ModelListError::Rpc)
                    });
                    finish(
                        shared,
                        queued.id,
                        SafeOperationResult::Failed("model_unavailable"),
                        LoginState::Idle,
                    );
                }
                Err(error) => {
                    mutate(shared, |snapshot| {
                        snapshot.model_list = ModelListState::Failed(error)
                    });
                    finish(
                        shared,
                        queued.id,
                        SafeOperationResult::Failed("model_unavailable"),
                        LoginState::Idle,
                    );
                }
            }
        }
        WorkerCommand::SaveSettings {
            expected_revision,
            settings: proposed,
        } => {
            save_settings_command(
                queued.id,
                expected_revision,
                proposed,
                paths,
                settings_store,
                journal,
                config,
                shared,
                state,
                settings,
            );
        }
        WorkerCommand::UpdateCustomCredential {
            expected_revision,
            old_credential_ref,
            settings_without_credential,
            secret,
        } => {
            update_custom_credential_command(
                queued.id,
                expected_revision,
                old_credential_ref,
                settings_without_credential,
                secret,
                paths,
                config,
                shared,
                state,
                settings,
            );
        }
        WorkerCommand::DeleteCustomCredential {
            expected_revision,
            old_credential_ref,
            settings_without_credential,
        } => {
            delete_custom_credential_command(
                queued.id,
                expected_revision,
                old_credential_ref,
                settings_without_credential,
                paths,
                config,
                shared,
                state,
                settings,
            );
        }
    }
}

#[allow(clippy::too_many_arguments)] // One serialized worker operation spans persistence and runtime application.
fn save_settings_command(
    id: OperationId,
    expected_revision: u64,
    proposed: AiSettings,
    paths: &AiPaths,
    settings_store: &AiSettingsStore,
    _journal: &CredentialJournal,
    config: &AiServiceConfig,
    shared: &Arc<SharedServiceState>,
    state: &mut WorkerState,
    settings: &mut AiSettings,
) {
    if !settings_save_allowed(shared) {
        finish(
            shared,
            id,
            SafeOperationResult::Failed("recovery_required"),
            LoginState::Idle,
        );
        return;
    }
    publish_busy(shared, id, ServiceBusyReason::Saving);
    let settings_path = settings_store.path().to_path_buf();
    let settings_lock_path = settings_store.settings_lock().path().to_path_buf();
    let owner_lock_path = settings_store.owner_lock_path().to_path_buf();
    let owned_paths = paths.clone();
    let save = with_state_owner(state, move |owner| {
        let store = AiSettingsStore::new(settings_path, settings_lock_path, owner_lock_path);
        let journal = CredentialJournal::new(owned_paths.credential_journal_path());
        store.save(owner, expected_revision, proposed, || journal.is_empty())
    });
    match save {
        Ok(Ok(saved)) => {
            finish_persisted_save(id, saved, false, paths, config, shared, state, settings)
        }
        Ok(Err(error)) => {
            handle_plain_save_error(id, error, paths, settings_store, shared, state, settings)
        }
        Err(()) => {
            mutate(shared, |snapshot| {
                snapshot.persistence = PersistenceState::NotCommitted
            });
            finish(
                shared,
                id,
                SafeOperationResult::Failed("owner_unavailable"),
                LoginState::Idle,
            );
        }
    }
}

#[allow(clippy::too_many_arguments)] // Keeps the credential transaction and its recovery context explicit.
fn update_custom_credential_command(
    id: OperationId,
    expected_revision: u64,
    old_credential_ref: Option<CredentialRef>,
    settings_without_credential: AiSettings,
    secret: String,
    paths: &AiPaths,
    config: &AiServiceConfig,
    shared: &Arc<SharedServiceState>,
    state: &mut WorkerState,
    settings: &mut AiSettings,
) {
    if !settings_actions_allowed(shared) {
        finish(
            shared,
            id,
            SafeOperationResult::Failed("recovery_required"),
            LoginState::Idle,
        );
        return;
    }
    publish_busy(shared, id, ServiceBusyReason::Saving);
    let paths_owned = paths.clone();
    let credential_store = config.credential_store.clone();
    let save = with_state_owner(state, move |owner| {
        let store = AiSettingsStore::new(
            paths_owned.settings_path(),
            paths_owned.settings_lock_path(),
            paths_owned.runtime_owner_lock_path(),
        );
        let journal = CredentialJournal::new(paths_owned.credential_journal_path());
        update_custom_credential_detailed(
            &store,
            owner,
            &journal,
            credential_store.as_ref(),
            ExpectedCredentialState {
                revision: expected_revision,
                credential_ref: old_credential_ref,
            },
            &secret,
            move |credential_ref| {
                let mut proposed = settings_without_credential;
                if let Some(custom) = proposed.custom.as_mut() {
                    custom.credential_ref = Some(credential_ref.clone());
                }
                proposed
            },
        )
    });
    match save {
        Ok(outcome) => {
            handle_credential_save_outcome(id, outcome, paths, config, shared, state, settings)
        }
        Err(()) => {
            mutate(shared, |snapshot| {
                snapshot.persistence = PersistenceState::NotCommitted
            });
            finish(
                shared,
                id,
                SafeOperationResult::Failed("owner_unavailable"),
                LoginState::Idle,
            );
        }
    }
}

#[allow(clippy::too_many_arguments)] // Keeps the credential transaction and its recovery context explicit.
fn delete_custom_credential_command(
    id: OperationId,
    expected_revision: u64,
    old_credential_ref: CredentialRef,
    settings_without_credential: AiSettings,
    paths: &AiPaths,
    config: &AiServiceConfig,
    shared: &Arc<SharedServiceState>,
    state: &mut WorkerState,
    settings: &mut AiSettings,
) {
    if !settings_actions_allowed(shared) {
        finish(
            shared,
            id,
            SafeOperationResult::Failed("recovery_required"),
            LoginState::Idle,
        );
        return;
    }
    publish_busy(shared, id, ServiceBusyReason::Saving);
    let paths_owned = paths.clone();
    let credential_store = config.credential_store.clone();
    let save = with_state_owner(state, move |owner| {
        let store = AiSettingsStore::new(
            paths_owned.settings_path(),
            paths_owned.settings_lock_path(),
            paths_owned.runtime_owner_lock_path(),
        );
        let journal = CredentialJournal::new(paths_owned.credential_journal_path());
        delete_custom_credential_detailed(
            &store,
            owner,
            &journal,
            credential_store.as_ref(),
            expected_revision,
            old_credential_ref,
            settings_without_credential,
        )
    });
    match save {
        Ok(outcome) => {
            handle_credential_save_outcome(id, outcome, paths, config, shared, state, settings)
        }
        Err(()) => {
            mutate(shared, |snapshot| {
                snapshot.persistence = PersistenceState::NotCommitted
            });
            finish(
                shared,
                id,
                SafeOperationResult::Failed("owner_unavailable"),
                LoginState::Idle,
            );
        }
    }
}

fn with_state_owner<R: Send + 'static>(
    state: &WorkerState,
    operation: impl FnOnce(&OwnerLockGuard) -> R + Send + 'static,
) -> Result<R, ()> {
    if let Some(owner) = state.owner_guard.as_ref() {
        return Ok(operation(owner));
    }
    let runtime = state.runtime.as_ref().ok_or(())?;
    runtime
        .with_owner_lock(move |owner| owner.map(operation))
        .map_err(|_| ())?
        .ok_or(())
}

fn handle_plain_save_error(
    id: OperationId,
    error: SaveError,
    paths: &AiPaths,
    settings_store: &AiSettingsStore,
    shared: &Arc<SharedServiceState>,
    state: &mut WorkerState,
    settings: &mut AiSettings,
) {
    match error {
        SaveError::RevisionConflict { current } => {
            *settings = *current;
            let current = settings.clone();
            mutate(shared, |snapshot| {
                snapshot.settings = current;
                snapshot.persistence = PersistenceState::NotCommitted;
            });
            finish(
                shared,
                id,
                SafeOperationResult::Failed("revision_conflict"),
                LoginState::Idle,
            );
        }
        SaveError::PendingCredentialJournal => {
            let current = settings_store.load().ok();
            if let Some(current) = current {
                *settings = current.clone();
                mutate(shared, |snapshot| snapshot.settings = current);
            }
            mutate(shared, |snapshot| {
                snapshot.recovery_required = true;
                snapshot.persistence = PersistenceState::RecoveryRequired;
            });
            finish(
                shared,
                id,
                SafeOperationResult::Failed("recovery_required"),
                LoginState::Idle,
            );
        }
        SaveError::PersistedDurabilityUnconfirmed(_) => {
            if let Ok(current) = settings_store.load() {
                *settings = current.clone();
                mutate(shared, |snapshot| snapshot.settings = current);
            }
            stop_runtime_for_recovery(state, shared);
            mutate(shared, |snapshot| {
                snapshot.recovery_required = true;
                snapshot.persistence = PersistenceState::DurabilityUnconfirmed;
            });
            finish(
                shared,
                id,
                SafeOperationResult::Failed("durability_unconfirmed"),
                LoginState::Idle,
            );
        }
        SaveError::CredentialRefMutationRequiresJournal => {
            mutate(shared, |snapshot| {
                snapshot.persistence = PersistenceState::NotCommitted
            });
            finish(
                shared,
                id,
                SafeOperationResult::Failed("credential_transaction_required"),
                LoginState::Idle,
            );
        }
        SaveError::Busy => {
            mutate(shared, |snapshot| {
                snapshot.persistence = PersistenceState::NotCommitted
            });
            finish(
                shared,
                id,
                SafeOperationResult::Failed("busy"),
                LoginState::Idle,
            );
        }
        _ => {
            let _ = paths;
            mutate(shared, |snapshot| {
                snapshot.persistence = PersistenceState::NotCommitted
            });
            finish(
                shared,
                id,
                SafeOperationResult::Failed("save_failed"),
                LoginState::Idle,
            );
        }
    }
}

fn handle_credential_save_outcome(
    id: OperationId,
    outcome: SaveOutcome,
    paths: &AiPaths,
    config: &AiServiceConfig,
    shared: &Arc<SharedServiceState>,
    state: &mut WorkerState,
    settings: &mut AiSettings,
) {
    match outcome {
        SaveOutcome::NotCommitted(error) => {
            if let ConnectError::Save(SaveError::RevisionConflict { current }) = &error {
                *settings = (**current).clone();
                let current = settings.clone();
                mutate(shared, |snapshot| snapshot.settings = current);
            }
            let code = match error {
                ConnectError::Save(SaveError::RevisionConflict { .. }) => "revision_conflict",
                ConnectError::Save(SaveError::PendingCredentialJournal) => "recovery_required",
                ConnectError::SettingsBusy | ConnectError::Save(SaveError::Busy) => "busy",
                ConnectError::CredentialStore(_) | ConnectError::CredentialNotFound => {
                    "credential_unavailable"
                }
                ConnectError::Provider(_) => "validation_failed",
                _ => "save_failed",
            };
            mutate(shared, |snapshot| {
                snapshot.persistence = if code == "recovery_required" {
                    snapshot.recovery_required = true;
                    PersistenceState::RecoveryRequired
                } else {
                    PersistenceState::NotCommitted
                };
            });
            finish(
                shared,
                id,
                SafeOperationResult::Failed(code),
                LoginState::Idle,
            );
        }
        SaveOutcome::Committed {
            settings: saved,
            cleanup_pending,
        } => finish_persisted_save(
            id,
            saved,
            cleanup_pending,
            paths,
            config,
            shared,
            state,
            settings,
        ),
        SaveOutcome::DurabilityUnconfirmed { observed, .. } => {
            if let Some(current) = observed {
                *settings = current.clone();
                mutate(shared, |snapshot| snapshot.settings = current);
            }
            stop_runtime_for_recovery(state, shared);
            mutate(shared, |snapshot| {
                snapshot.recovery_required = true;
                snapshot.persistence = PersistenceState::DurabilityUnconfirmed;
            });
            finish(
                shared,
                id,
                SafeOperationResult::Failed("durability_unconfirmed"),
                LoginState::Idle,
            );
        }
    }
}

#[allow(clippy::too_many_arguments)] // Commit state, runtime application, and recovery state share one worker operation.
fn finish_persisted_save(
    id: OperationId,
    saved: AiSettings,
    cleanup_pending: bool,
    paths: &AiPaths,
    config: &AiServiceConfig,
    shared: &Arc<SharedServiceState>,
    state: &mut WorkerState,
    settings: &mut AiSettings,
) {
    let previous = settings.clone();
    let runtime_affecting = previous.settings_generation != saved.settings_generation;
    let connection_changed = previous.active_connection != saved.active_connection;
    *settings = saved.clone();
    mutate(shared, |snapshot| {
        snapshot.settings = saved.clone();
        snapshot.recovery_required = cleanup_pending;
        snapshot.persistence = if cleanup_pending {
            PersistenceState::CleanupPending
        } else {
            PersistenceState::Saved
        };
        if connection_changed {
            snapshot.account = AccountState::Unknown;
            snapshot.account_refresh_failed = false;
            snapshot.model_list = ModelListState::NotLoaded;
            snapshot.auth_epoch = snapshot.auth_epoch.saturating_add(1);
        }
    });

    if cleanup_pending {
        stop_runtime_for_recovery(state, shared);
        finish(
            shared,
            id,
            SafeOperationResult::Failed("cleanup_pending"),
            LoginState::Idle,
        );
        return;
    }

    let mut apply_failed = false;
    if runtime_affecting
        && let Some(runtime) = state.runtime.as_ref()
        && runtime.snapshot().state == RuntimeState::Ready
    {
        let new_config = build_runtime_config_for_active_connection(
            &saved,
            config.credential_store.as_ref(),
            paths,
            config.binary_path.clone(),
            paths.runtime_owner_lock_path(),
            config.shell_env_format,
        );
        match new_config {
            Ok(configured) => {
                if runtime
                    .reconfigure(configured.config, configured.settings_generation)
                    .is_err()
                {
                    apply_failed = true;
                }
            }
            Err(_) => {
                apply_failed = true;
                let _ = runtime.stop();
            }
        }
    }
    if let Some(runtime) = state.runtime.as_ref() {
        sync_runtime_snapshot(shared, runtime);
    }
    finish(
        shared,
        id,
        if apply_failed {
            SafeOperationResult::Failed("apply_failed")
        } else {
            SafeOperationResult::Succeeded
        },
        LoginState::Idle,
    );
}

fn stop_runtime_for_recovery(state: &mut WorkerState, shared: &Arc<SharedServiceState>) {
    if let Some(runtime) = state.runtime.as_ref() {
        let _ = runtime.stop();
        sync_runtime_snapshot(shared, runtime);
    }
}

fn sync_runtime_snapshot(shared: &Arc<SharedServiceState>, runtime: &AiRuntime) {
    let status = runtime.snapshot();
    let configured = runtime.configured_settings_generation();
    mutate(shared, |snapshot| {
        snapshot.runtime_state = status.state;
        snapshot.runtime_generation = status.generation;
        snapshot.configured_settings_generation = configured;
    });
}

enum OwnerOpenError {
    Elsewhere,
    Failed,
}

fn open_settings(
    paths: &AiPaths,
    settings_store: &AiSettingsStore,
    journal: &CredentialJournal,
    credential_store: Arc<dyn CredentialStore>,
    state: &mut WorkerState,
) -> Result<(AiSettings, bool), OwnerOpenError> {
    let runtime_owns = if let Some(runtime) = state.runtime.as_ref() {
        if runtime.snapshot().restart_blocked {
            return Err(OwnerOpenError::Failed);
        }
        runtime
            .with_owner_lock(|owner| owner.is_some())
            .map_err(|_| OwnerOpenError::Failed)?
    } else {
        false
    };
    if state.owner_guard.is_none() && !runtime_owns {
        let lock = OwnerLock::new(paths.runtime_owner_lock_path());
        state.owner_guard = Some(
            lock.try_acquire()
                .map_err(|_| OwnerOpenError::Failed)?
                .ok_or(OwnerOpenError::Elsewhere)?,
        );
    }
    if let Some(owner) = state.owner_guard.as_ref() {
        let recovery =
            recover_at_startup(settings_store, owner, journal, credential_store.as_ref())
                .map_err(|_| OwnerOpenError::Failed)?;
        let reloaded = settings_store.load().map_err(|_| OwnerOpenError::Failed)?;
        let pending = recovery.left_for_diagnosis > 0
            || recovery.deletion_failed > 0
            || recovery.pending_settings_swap > 0
            || !journal.is_empty().map_err(|_| OwnerOpenError::Failed)?;
        Ok((reloaded, pending))
    } else {
        let runtime = state.runtime.as_ref().ok_or(OwnerOpenError::Failed)?;
        if !runtime_owns {
            return Err(OwnerOpenError::Failed);
        }
        let settings_path = settings_store.path().to_path_buf();
        let settings_lock_path = settings_store.settings_lock().path().to_path_buf();
        let owner_lock_path = settings_store.owner_lock_path().to_path_buf();
        let paths = paths.clone();
        runtime
            .with_owner_lock(move |owner| {
                let Some(owner) = owner else {
                    return Err(OwnerOpenError::Failed);
                };
                let scoped_store =
                    AiSettingsStore::new(settings_path, settings_lock_path, owner_lock_path);
                let scoped_journal = CredentialJournal::new(paths.credential_journal_path());
                let recovery = recover_at_startup(
                    &scoped_store,
                    owner,
                    &scoped_journal,
                    credential_store.as_ref(),
                )
                .map_err(|_| OwnerOpenError::Failed)?;
                let current = scoped_store.load().map_err(|_| OwnerOpenError::Failed)?;
                let pending = recovery.left_for_diagnosis > 0
                    || recovery.deletion_failed > 0
                    || recovery.pending_settings_swap > 0
                    || !scoped_journal
                        .is_empty()
                        .map_err(|_| OwnerOpenError::Failed)?;
                Ok((current, pending))
            })
            .map_err(|_| OwnerOpenError::Failed)?
    }
}

fn ensure_runtime<'a>(
    config: &AiServiceConfig,
    paths: &AiPaths,
    settings: &AiSettings,
    state: &'a mut WorkerState,
) -> Result<&'a AiRuntime, ()> {
    let needs_config = state.runtime.as_ref().is_none_or(|runtime| {
        runtime.configured_settings_generation() != settings.settings_generation
    });
    if needs_config {
        let configured = build_runtime_config_for_active_connection(
            settings,
            config.credential_store.as_ref(),
            paths,
            config.binary_path.clone(),
            paths.runtime_owner_lock_path(),
            config.shell_env_format,
        );
        let configured = match configured {
            Ok(configured) => configured,
            Err(_) => return Err(()),
        };
        if let Some(runtime) = state.runtime.as_ref() {
            let status = runtime.snapshot();
            if status.restart_blocked {
                return Err(());
            }
            if state.owner_guard.is_some() && status.state == RuntimeState::Stopped {
                // `OpenSettings` may have reacquired the owner lock after a
                // prior stop. Keep that same guard across replacement of the
                // stale coordinator and hand it to the new runtime at start.
                let old_runtime = state.runtime.take();
                drop(old_runtime);
                let (event_tx, event_rx) = mpsc::sync_channel(128);
                let runtime = AiRuntime::spawn_with_configured_settings_generation(
                    configured.config,
                    configured.settings_generation,
                    Arc::new(RejectAllServerRequests),
                    event_tx,
                );
                state.runtime_events = Some(event_rx);
                let owner = state.owner_guard.take().ok_or(())?;
                runtime.start_with_owner_lock(owner).map_err(|_| ())?;
                state.runtime = Some(runtime);
            } else {
                runtime
                    .reconfigure(configured.config, configured.settings_generation)
                    .map_err(|_| ())?;
            }
        } else {
            let (event_tx, event_rx) = mpsc::sync_channel(128);
            let runtime = AiRuntime::spawn_with_configured_settings_generation(
                configured.config,
                configured.settings_generation,
                Arc::new(RejectAllServerRequests),
                event_tx,
            );
            state.runtime_events = Some(event_rx);
            state.runtime = Some(runtime);
        }
    }
    let runtime = state.runtime.as_ref().ok_or(())?;
    if runtime.snapshot().state != RuntimeState::Ready {
        let result = if let Some(owner) = state.owner_guard.take() {
            runtime.start_with_owner_lock(owner)
        } else {
            runtime.start()
        };
        if result.is_err() {
            return Err(());
        }
    }
    state.runtime.as_ref().ok_or(())
}

fn chatgpt_active(settings: &AiSettings) -> bool {
    settings.active_connection == ActiveConnection::ChatGpt
}

fn settings_actions_allowed(shared: &Arc<SharedServiceState>) -> bool {
    let snapshot = shared.snapshot.lock().unwrap();
    snapshot.ownership == OwnershipState::Owned && !snapshot.recovery_required
}

fn settings_save_allowed(shared: &Arc<SharedServiceState>) -> bool {
    let snapshot = shared.snapshot.lock().unwrap();
    snapshot.ownership == OwnershipState::Owned
        && (!snapshot.recovery_required
            || snapshot.persistence == PersistenceState::DurabilityUnconfirmed)
}

fn call_account_read(runtime: &AiRuntime, refresh_token: bool) -> Result<AccountState, ()> {
    let response = runtime
        .call(
            "account/read",
            Some(account_read_params(refresh_token)),
            RPC_TIMEOUT,
        )
        .map_err(|_| ())?;
    parse_account_read(&response).map_err(|_| ())
}

fn start_login(
    id: OperationId,
    config: &AiServiceConfig,
    paths: &AiPaths,
    settings: &AiSettings,
    shared: &Arc<SharedServiceState>,
    state: &mut WorkerState,
) {
    if !settings_actions_allowed(shared) {
        finish(
            shared,
            id,
            SafeOperationResult::Failed("recovery_required"),
            LoginState::Idle,
        );
        return;
    }
    if !chatgpt_active(settings) {
        finish(
            shared,
            id,
            SafeOperationResult::Failed("chatgpt_not_active"),
            LoginState::Idle,
        );
        return;
    }
    publish_busy(shared, id, ServiceBusyReason::Login);
    mutate(shared, |snapshot| snapshot.login = LoginState::Starting);
    let runtime = match ensure_runtime(config, paths, settings, state) {
        Ok(runtime) => runtime,
        Err(()) => {
            finish(
                shared,
                id,
                SafeOperationResult::Failed("runtime_unavailable"),
                LoginState::Idle,
            );
            return;
        }
    };
    let account = match call_account_read(runtime, false) {
        Ok(account) => account,
        Err(()) => {
            mutate(shared, |snapshot| snapshot.account_refresh_failed = true);
            finish(
                shared,
                id,
                SafeOperationResult::Failed("account_unknown"),
                LoginState::Idle,
            );
            return;
        }
    };
    match account {
        AccountState::SignedOut => {}
        AccountState::SignedIn { .. } => {
            mutate(shared, |snapshot| {
                if snapshot.account != account {
                    snapshot.auth_epoch += 1;
                }
                snapshot.account = account;
                snapshot.account_refresh_failed = false;
            });
            finish(
                shared,
                id,
                SafeOperationResult::Succeeded,
                LoginState::Finished,
            );
            return;
        }
        AccountState::Unknown | AccountState::ApiKey => {
            mutate(shared, |snapshot| {
                snapshot.account = account;
                snapshot.account_refresh_failed = false;
            });
            finish(
                shared,
                id,
                SafeOperationResult::Failed("account_unknown"),
                LoginState::Idle,
            );
            return;
        }
    }
    let result = state.runtime.as_ref().ok_or(()).and_then(|runtime| {
        runtime
            .call(
                "account/login/start",
                Some(json!({"type":"chatgpt"})),
                RPC_TIMEOUT,
            )
            .map_err(|_| ())
    });
    let start = match result.and_then(|value| parse_chatgpt_login_start(&value).map_err(|_| ())) {
        Ok(start) => start,
        Err(()) => {
            finish(
                shared,
                id,
                SafeOperationResult::Failed("login_failed"),
                LoginState::Idle,
            );
            return;
        }
    };
    let runtime = match state.runtime.as_ref() {
        Some(runtime) => runtime,
        None => {
            finish(
                shared,
                id,
                SafeOperationResult::Failed("runtime_unavailable"),
                LoginState::Idle,
            );
            return;
        }
    };
    let status = runtime.snapshot();
    let attempt = LoginAttempt {
        id,
        login_id: start.login_id.clone(),
        runtime_generation: status.generation,
        settings_generation: settings.settings_generation,
        deadline: Instant::now() + LOGIN_TIMEOUT,
    };

    // Notifications may be read before the response that reveals loginId.
    // Keep only this short, bounded period and reconcile after the ID is
    // known; no notification is treated as success without account/read.
    drain_early_login_events(state);
    state.attempt = Some(attempt);
    mutate(shared, |snapshot| {
        snapshot.login = LoginState::AwaitingBrowser
    });
    if config
        .browser_opener
        .open(start.authorization_url.as_str())
        .is_err()
    {
        state.attempt.as_mut().unwrap().deadline = Instant::now();
        handle_cancel(
            id,
            config,
            paths,
            &dummy_settings_store(paths),
            settings,
            shared,
            state,
            SafeOperationResult::Failed("browser_unavailable"),
        );
        return;
    }
    reconcile_early_login_events(config, paths, settings, shared, state);
}

fn dummy_settings_store(paths: &AiPaths) -> AiSettingsStore {
    AiSettingsStore::new(
        paths.settings_path(),
        paths.settings_lock_path(),
        paths.runtime_owner_lock_path(),
    )
}

fn drain_early_login_events(state: &mut WorkerState) {
    let Some(events) = state.runtime_events.as_ref() else {
        return;
    };
    loop {
        match events.try_recv() {
            Ok(RuntimeEvent {
                generation,
                kind: RuntimeEventKind::Notification { method, params },
            }) if method == "account/login/completed" => {
                let size = method.len()
                    + params
                        .as_ref()
                        .map_or(0, |value| serde_json::to_vec(value).map_or(0, |v| v.len()));
                if state.early_login_events.len() >= MAX_EARLY_LOGIN_EVENTS
                    || state.early_login_bytes.saturating_add(size) > MAX_EARLY_LOGIN_BYTES
                {
                    state.early_login_events.clear();
                    state.early_login_bytes = MAX_EARLY_LOGIN_BYTES + 1;
                    break;
                }
                state.early_login_bytes += size;
                state
                    .early_login_events
                    .push_back((generation, params, size));
            }
            Ok(_) => {}
            Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
        }
    }
}

fn reconcile_early_login_events(
    config: &AiServiceConfig,
    paths: &AiPaths,
    settings: &AiSettings,
    shared: &Arc<SharedServiceState>,
    state: &mut WorkerState,
) {
    if state.early_login_bytes > MAX_EARLY_LOGIN_BYTES {
        if let Some(attempt) = state.attempt.as_ref() {
            handle_cancel(
                attempt.id,
                config,
                paths,
                &dummy_settings_store(paths),
                settings,
                shared,
                state,
                SafeOperationResult::Failed("notification_overflow"),
            );
        }
        return;
    }
    while let Some((generation, params, size)) = state.early_login_events.pop_front() {
        state.early_login_bytes = state.early_login_bytes.saturating_sub(size);
        if let Some(attempt) = state.attempt.as_ref()
            && login_notification_matches_attempt(
                attempt,
                generation,
                settings.settings_generation,
                params.as_ref(),
            )
        {
            finish_login_reconciliation(attempt.id, settings, shared, state);
            break;
        }
    }
}

fn drain_runtime_events(
    _config: &AiServiceConfig,
    _paths: &AiPaths,
    _settings_store: &AiSettingsStore,
    settings: &AiSettings,
    shared: &Arc<SharedServiceState>,
    state: &mut WorkerState,
) {
    let mut ready = Vec::new();
    if let Some(events) = state.runtime_events.as_ref() {
        while let Ok(event) = events.try_recv() {
            ready.push(event);
        }
    }
    let current_generation = state
        .runtime
        .as_ref()
        .map(|runtime| runtime.snapshot().generation);
    let mut account_updated = false;
    for event in ready {
        if Some(event.generation) != current_generation {
            continue;
        }
        if let RuntimeEventKind::Notification { method, params } = event.kind {
            if method == "account/updated" {
                account_updated = true;
            }
            let completed_attempt_id = state.attempt.as_ref().and_then(|attempt| {
                (method == "account/login/completed"
                    && login_notification_matches_attempt(
                        attempt,
                        event.generation,
                        settings.settings_generation,
                        params.as_ref(),
                    ))
                .then_some(attempt.id)
            });
            if let Some(id) = completed_attempt_id {
                finish_login_reconciliation(id, settings, shared, state);
            }
        }
    }
    if state.attempt.is_none()
        && account_updated
        && chatgpt_active(settings)
        && shared.busy.lock().unwrap().is_none()
        && let Some(runtime) = state.runtime.as_ref()
    {
        match call_account_read(runtime, false) {
            Ok(account) => mutate(shared, |snapshot| {
                if snapshot.account != account {
                    snapshot.auth_epoch += 1;
                }
                snapshot.account = account;
                snapshot.account_refresh_failed = false;
            }),
            Err(()) => mutate(shared, |snapshot| snapshot.account_refresh_failed = true),
        }
    }
    if let Some(runtime) = state.runtime.as_ref() {
        let status = runtime.snapshot();
        let configured = runtime.configured_settings_generation();
        let before = shared.snapshot.lock().unwrap().clone();
        if before.runtime_state != status.state
            || before.runtime_generation != status.generation
            || before.configured_settings_generation != configured
        {
            mutate(shared, |snapshot| {
                snapshot.runtime_state = status.state;
                snapshot.runtime_generation = status.generation;
                snapshot.configured_settings_generation = configured;
            });
        }
        if let Some(attempt) = state.attempt.as_ref()
            && (attempt.runtime_generation != status.generation
                || attempt.settings_generation != settings.settings_generation
                || status.state != RuntimeState::Ready)
        {
            let id = attempt.id;
            state.attempt = None;
            finish(
                shared,
                id,
                SafeOperationResult::Failed("stale_runtime"),
                LoginState::Finished,
            );
        }
    }
}

fn finish_login_reconciliation(
    id: OperationId,
    _settings: &AiSettings,
    shared: &Arc<SharedServiceState>,
    state: &mut WorkerState,
) {
    let Some(runtime) = state.runtime.as_ref() else {
        return;
    };
    mutate(shared, |snapshot| snapshot.login = LoginState::Reconciling);
    let account = call_account_read(runtime, false);
    match account {
        Ok(account) => {
            let signed_in = matches!(account, AccountState::SignedIn { .. });
            mutate(shared, |snapshot| {
                if snapshot.account != account {
                    snapshot.auth_epoch += 1;
                }
                snapshot.account = account;
                snapshot.account_refresh_failed = false;
                snapshot.model_list = ModelListState::NotLoaded;
            });
            if signed_in {
                mutate(shared, |snapshot| {
                    snapshot.model_list = ModelListState::Loading
                });
                let models = crate::models::fetch_chatgpt_models(|method, params| {
                    runtime
                        .call(method, Some(params), RPC_TIMEOUT)
                        .map_err(|_| ())
                });
                mutate(shared, |snapshot| {
                    snapshot.model_list = match models {
                        Ok(models) => ModelListState::Loaded(models),
                        Err(error) => ModelListState::Failed(error),
                    }
                });
            }
            state.attempt = None;
            finish(
                shared,
                id,
                if signed_in {
                    SafeOperationResult::Succeeded
                } else {
                    SafeOperationResult::Failed("login_unconfirmed")
                },
                LoginState::Finished,
            );
        }
        Err(()) => {
            mutate(shared, |snapshot| snapshot.account_refresh_failed = true);
            state.attempt = None;
            finish(
                shared,
                id,
                SafeOperationResult::Failed("account_unknown"),
                LoginState::Finished,
            );
        }
    }
}

fn login_notification_matches_attempt(
    attempt: &LoginAttempt,
    runtime_generation: u64,
    settings_generation: u64,
    params: Option<&Value>,
) -> bool {
    attempt.runtime_generation == runtime_generation
        && attempt.settings_generation == settings_generation
        && completed_login_matches(params, &attempt.login_id).is_some()
}

fn cancel_terminal_result(
    cancel: &Result<CancelStatus, ()>,
    account: &Result<AccountState, ()>,
    result_if_signed_out: &SafeOperationResult,
) -> SafeOperationResult {
    match account {
        Ok(AccountState::SignedIn { .. }) => SafeOperationResult::Succeeded,
        Ok(AccountState::SignedOut) => {
            if matches!(cancel, Ok(CancelStatus::Canceled | CancelStatus::NotFound)) {
                result_if_signed_out.clone()
            } else {
                SafeOperationResult::Failed("login_unknown")
            }
        }
        Ok(AccountState::Unknown | AccountState::ApiKey) | Err(()) => {
            SafeOperationResult::Failed("account_unknown")
        }
    }
}

#[allow(clippy::too_many_arguments)] // Cancellation reconciliation is serialized with the login attempt and runtime.
fn handle_cancel(
    id: OperationId,
    _config: &AiServiceConfig,
    _paths: &AiPaths,
    _settings_store: &AiSettingsStore,
    _settings: &AiSettings,
    shared: &Arc<SharedServiceState>,
    state: &mut WorkerState,
    result_if_signed_out: SafeOperationResult,
) {
    let Some(attempt) = state.attempt.as_ref() else {
        return;
    };
    if attempt.id != id {
        return;
    }
    let login_id = attempt.login_id.clone();
    let Some(runtime) = state.runtime.as_ref() else {
        return;
    };
    mutate(shared, |snapshot| snapshot.login = LoginState::Canceling);
    let cancel = runtime
        .call(
            "account/login/cancel",
            Some(json!({"loginId": login_id})),
            RPC_TIMEOUT,
        )
        .map_err(|_| ())
        .and_then(|value| parse_cancel_status(&value).map_err(|_| ()));
    let account = call_account_read(runtime, false);
    let terminal_result = cancel_terminal_result(&cancel, &account, &result_if_signed_out);
    state.attempt = None;
    match account {
        Ok(account) if matches!(account, AccountState::SignedIn { .. }) => {
            mutate(shared, |snapshot| {
                snapshot.account = account;
                snapshot.auth_epoch += 1;
                snapshot.account_refresh_failed = false;
            });
            finish(
                shared,
                id,
                SafeOperationResult::Succeeded,
                LoginState::Finished,
            );
        }
        Ok(account) if matches!(account, AccountState::SignedOut) => {
            mutate(shared, |snapshot| {
                snapshot.account = account;
                snapshot.account_refresh_failed = false;
            });
            finish(shared, id, terminal_result, LoginState::Finished);
        }
        Ok(account) => {
            mutate(shared, |snapshot| {
                snapshot.account = account;
                snapshot.account_refresh_failed = true;
            });
            finish(
                shared,
                id,
                SafeOperationResult::Failed("account_unknown"),
                LoginState::Finished,
            );
        }
        Err(()) => {
            mutate(shared, |snapshot| snapshot.account_refresh_failed = true);
            finish(
                shared,
                id,
                SafeOperationResult::Failed("account_unknown"),
                LoginState::Finished,
            );
        }
    }
}

fn handle_timeout(
    id: OperationId,
    config: &AiServiceConfig,
    paths: &AiPaths,
    settings_store: &AiSettingsStore,
    settings: &AiSettings,
    shared: &Arc<SharedServiceState>,
    state: &mut WorkerState,
) {
    handle_cancel(
        id,
        config,
        paths,
        settings_store,
        settings,
        shared,
        state,
        SafeOperationResult::TimedOut,
    );
}

fn publish_busy(shared: &Arc<SharedServiceState>, id: OperationId, reason: ServiceBusyReason) {
    let mut busy = shared.busy.lock().unwrap();
    *busy = Some((id, reason));
    mutate(shared, |snapshot| snapshot.busy = Some((id, reason)));
}

fn finish(
    shared: &Arc<SharedServiceState>,
    id: OperationId,
    result: SafeOperationResult,
    login: LoginState,
) {
    let mut busy = shared.busy.lock().unwrap();
    *busy = None;
    mutate(shared, |snapshot| {
        snapshot.busy = None;
        snapshot.login = login;
        snapshot.last_result = Some((id, result));
    });
    drop(busy);
}

fn mutate(shared: &Arc<SharedServiceState>, apply: impl FnOnce(&mut AiSnapshot)) {
    let snapshot = {
        let mut snapshot = shared.snapshot.lock().unwrap();
        apply(&mut snapshot);
        snapshot.state_version = snapshot.state_version.saturating_add(1);
        snapshot.clone()
    };
    let mut subscribers = shared.subscribers.lock().unwrap();
    subscribers.retain(|subscriber| match subscriber.try_send(snapshot.clone()) {
        Ok(()) | Err(TrySendError::Full(_)) => true,
        Err(TrySendError::Disconnected(_)) => false,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_admission_is_bounded_and_cancellation_uses_a_separate_gate() {
        let (command_tx, _command_rx) = mpsc::sync_channel(1);
        let (cancel_tx, _cancel_rx) = mpsc::sync_channel(1);
        let shared = Arc::new(SharedServiceState {
            snapshot: Mutex::new(AiSnapshot::default()),
            busy: Mutex::new(None),
            next_operation_id: AtomicU64::new(1),
            subscribers: Mutex::new(Vec::new()),
            available: Mutex::new(true),
        });
        let handle = AiServiceHandle {
            command_tx,
            cancel_tx,
            shared,
        };
        let id = handle.try_submit(AiCommand::OpenSettings).unwrap();
        assert_eq!(
            handle.try_submit(AiCommand::RefreshModels),
            Err(AdmissionError::Busy)
        );
        assert_eq!(handle.cancel(id), Err(AdmissionError::WrongOperation));
    }

    #[test]
    fn login_notifications_require_matching_attempt_and_both_generations() {
        let attempt = LoginAttempt {
            id: OperationId(7),
            login_id: "login-7".to_string(),
            runtime_generation: 3,
            settings_generation: 11,
            deadline: Instant::now() + Duration::from_secs(5),
        };
        let matching = serde_json::json!({"loginId":"login-7", "success":true});
        assert!(login_notification_matches_attempt(
            &attempt,
            3,
            11,
            Some(&matching)
        ));
        assert!(!login_notification_matches_attempt(
            &attempt,
            4,
            11,
            Some(&matching)
        ));
        assert!(!login_notification_matches_attempt(
            &attempt,
            3,
            12,
            Some(&matching)
        ));
        assert!(!login_notification_matches_attempt(
            &attempt,
            3,
            11,
            Some(&serde_json::json!({"loginId":"older-login", "success":true}))
        ));
    }

    #[test]
    fn cancellation_result_reconciles_account_before_using_cancel_status() {
        let not_found = Ok(CancelStatus::NotFound);
        let signed_in = Ok(AccountState::SignedIn {
            email: None,
            plan_type: "plus".to_string(),
        });
        assert_eq!(
            cancel_terminal_result(&not_found, &signed_in, &SafeOperationResult::Canceled),
            SafeOperationResult::Succeeded,
            "account/read wins when login completes during cancel"
        );

        let signed_out = Ok(AccountState::SignedOut);
        assert_eq!(
            cancel_terminal_result(&not_found, &signed_out, &SafeOperationResult::Canceled),
            SafeOperationResult::Canceled
        );
        assert_eq!(
            cancel_terminal_result(&not_found, &signed_out, &SafeOperationResult::TimedOut),
            SafeOperationResult::TimedOut
        );
        assert_eq!(
            cancel_terminal_result(&Err(()), &signed_out, &SafeOperationResult::Canceled),
            SafeOperationResult::Failed("login_unknown")
        );
        assert_eq!(
            cancel_terminal_result(
                &not_found,
                &Ok(AccountState::Unknown),
                &SafeOperationResult::Canceled
            ),
            SafeOperationResult::Failed("account_unknown")
        );
    }
}
