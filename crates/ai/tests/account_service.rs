use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use hane_ai::{
    AccountState, ActiveConnection, AdmissionError, AiCommand, AiService, AiServiceConfig,
    AiSettingsLock, BrowserOpenError, BrowserOpener, CredentialStore, CustomConnectionSettings,
    FakeCredentialStore, LoginState, ModelListState, OperationId, OwnerLock, OwnershipState,
    PersistenceState, ProbeErrorCode, ProbeStatus, SafeOperationResult, ServiceBusyReason,
    ShellEnvironmentPolicyFormat,
};

#[derive(Default)]
struct RecordingBrowser(Mutex<Vec<String>>);

impl BrowserOpener for RecordingBrowser {
    fn open(&self, url: &str) -> Result<(), BrowserOpenError> {
        self.0.lock().unwrap().push(url.to_owned());
        Ok(())
    }
}

struct FailingBrowser;

impl BrowserOpener for FailingBrowser {
    fn open(&self, _url: &str) -> Result<(), BrowserOpenError> {
        Err(BrowserOpenError)
    }
}

fn wait_for_result(service: &AiService, id: OperationId) -> (SafeOperationResult, LoginState) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let snapshot = service.handle().snapshot();
        if let Some((finished_id, result)) = &snapshot.last_result
            && *finished_id == id
            && snapshot.busy.is_none()
        {
            return (result.clone(), snapshot.login);
        }
        assert!(
            Instant::now() < deadline,
            "operation did not reach a terminal state: {snapshot:?}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn unique_data_root() -> PathBuf {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "hane-ai-account-service-{}-{id}",
        std::process::id()
    ))
}

fn configure_fake_scenario(root: &std::path::Path, scenario: &str) {
    let codex_home = root.join("ai/codex-chatgpt");
    std::fs::create_dir_all(&codex_home).unwrap();
    std::fs::write(codex_home.join(".hane-fake-app-server-scenario"), scenario).unwrap();
}

fn observed_fake_codex_home(root: &std::path::Path) -> Option<String> {
    std::fs::read_dir(root.join("ai/probe-workspace"))
        .ok()?
        .filter_map(Result::ok)
        .find_map(|entry| {
            std::fs::read_to_string(entry.path().join(".hane-fake-app-server-codex-home")).ok()
        })
}

fn spawn_service(root: PathBuf, browser_opener: Arc<dyn BrowserOpener>) -> AiService {
    AiService::spawn(AiServiceConfig {
        app_data_root: root,
        binary_path: PathBuf::from(env!("CARGO_BIN_EXE_fake_app_server")),
        credential_store: Arc::new(FakeCredentialStore::new()),
        browser_opener,
        shell_env_format: ShellEnvironmentPolicyFormat::Filters,
    })
    .unwrap()
}

fn open_settings(service: &AiService) {
    let id = service
        .handle()
        .try_submit(AiCommand::OpenSettings)
        .unwrap();
    assert_eq!(
        wait_for_result(service, id).0,
        SafeOperationResult::Succeeded
    );
}

fn prepare_probe_service(scenario: &str) -> (AiService, PathBuf) {
    let data_root = unique_data_root();
    configure_fake_scenario(&data_root, scenario);
    std::fs::write(
        data_root.join("ai/codex-chatgpt/.hane-fake-app-server-signed-in"),
        b"signed-in",
    )
    .unwrap();
    let service = spawn_service(data_root.clone(), Arc::new(RecordingBrowser::default()));
    open_settings(&service);

    let handle = service.handle();
    let mut settings = handle.snapshot().settings;
    settings.chatgpt.model_id = Some("gpt-fake-text".to_owned());
    let save_id = handle
        .try_save_settings(settings.revision, settings)
        .unwrap();
    assert_eq!(
        wait_for_result(&service, save_id).0,
        SafeOperationResult::Succeeded
    );

    let account_id = handle
        .try_submit(AiCommand::RefreshAccount {
            refresh_token: false,
        })
        .unwrap();
    assert_eq!(
        wait_for_result(&service, account_id).0,
        SafeOperationResult::Succeeded
    );
    assert!(matches!(
        handle.snapshot().account,
        AccountState::SignedIn { .. }
    ));

    let models_id = handle.try_submit(AiCommand::RefreshModels).unwrap();
    assert_eq!(
        wait_for_result(&service, models_id).0,
        SafeOperationResult::Succeeded
    );
    assert!(matches!(
        handle.snapshot().model_list,
        ModelListState::Loaded(_)
    ));
    (service, data_root)
}

fn wait_for_probe_record(root: &std::path::Path, needle: &str) -> String {
    let path = root.join("ai/codex-chatgpt/.hane-fake-app-server-probe-record");
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let record = std::fs::read_to_string(&path).unwrap_or_default();
        if record.contains(needle) {
            return record;
        }
        assert!(
            Instant::now() < deadline,
            "probe fixture did not record {needle:?}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn wait_until_login_is_waiting(service: &AiService) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if service.handle().snapshot().login == LoginState::AwaitingBrowser {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "login did not reach browser state"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn app_service_handles_early_login_completion_models_logout_and_view_recreation() {
    let browser = Arc::new(RecordingBrowser::default());
    let credential_store = Arc::new(FakeCredentialStore::new());
    let data_root = unique_data_root();
    let service = AiService::spawn(AiServiceConfig {
        app_data_root: data_root.clone(),
        binary_path: PathBuf::from(env!("CARGO_BIN_EXE_fake_app_server")),
        credential_store: credential_store.clone(),
        browser_opener: browser.clone(),
        shell_env_format: ShellEnvironmentPolicyFormat::Filters,
    })
    .unwrap();
    let handle = service.handle();

    let open_id = handle.try_submit(AiCommand::OpenSettings).unwrap();
    assert_eq!(
        wait_for_result(&service, open_id).0,
        SafeOperationResult::Succeeded
    );

    let mut draft = handle.snapshot().settings;
    draft.chatgpt.model_id = Some("saved-but-removed-model".to_string());
    draft.custom = Some(CustomConnectionSettings {
        id: "custom-connection".to_string(),
        name: "Local mock".to_string(),
        base_url: "http://127.0.0.1:4312/v1".to_string(),
        model_id: "local-model".to_string(),
        credential_ref: None,
    });
    let save_id = handle.try_save_settings(0, draft).unwrap();
    assert_eq!(
        wait_for_result(&service, save_id).0,
        SafeOperationResult::Succeeded
    );
    let saved = handle.snapshot();
    assert_eq!(saved.settings.settings_generation, 1);
    assert_eq!(saved.persistence, PersistenceState::Saved);
    assert_eq!(saved.runtime_state, hane_ai::RuntimeState::Stopped);
    assert_eq!(saved.settings.active_connection, ActiveConnection::ChatGpt);

    let mut renamed = saved.settings.clone();
    renamed.custom.as_mut().unwrap().name = "Renamed mock".to_string();
    let rename_id = handle
        .try_save_settings(saved.settings.revision, renamed)
        .unwrap();
    assert_eq!(
        wait_for_result(&service, rename_id).0,
        SafeOperationResult::Succeeded
    );
    assert_eq!(
        handle.snapshot().settings.settings_generation,
        saved.settings.settings_generation,
        "a display-name-only save must not change the runtime generation"
    );
    assert_eq!(
        handle.snapshot().runtime_state,
        hane_ai::RuntimeState::Stopped,
        "saving settings must not start the AI runtime or run inference"
    );

    // A settings page can disappear while the application-owned service
    // continues its operation. A new page subscribes to the current snapshot.
    let closed_view = handle.subscribe();
    drop(closed_view);
    let login_id = handle.try_submit(AiCommand::StartLogin).unwrap();
    assert_eq!(
        wait_for_result(&service, login_id),
        (SafeOperationResult::Succeeded, LoginState::Finished),
        "snapshot after login attempt: {:?}",
        handle.snapshot()
    );

    let snapshot = handle.snapshot();
    assert!(matches!(snapshot.account, AccountState::SignedIn { .. }));
    let expected_codex_home = data_root.join("ai").join("codex-chatgpt");
    assert_eq!(
        observed_fake_codex_home(&data_root).as_deref(),
        expected_codex_home.to_str(),
        "the child must receive Hane's dedicated CODEX_HOME"
    );
    assert_eq!(
        snapshot.settings.chatgpt.model_id.as_deref(),
        Some("saved-but-removed-model"),
        "fetching the catalog must not replace a saved model that disappeared"
    );
    assert!(
        matches!(snapshot.model_list, ModelListState::Loaded(ref models) if models.len() == 1 && models[0].model == "gpt-fake-text")
    );
    assert_eq!(snapshot.runtime_generation, 1);
    assert_eq!(snapshot.configured_settings_generation, 1);
    let urls = browser.0.lock().unwrap();
    assert_eq!(urls.len(), 1);
    assert!(urls[0].starts_with("https://auth.openai.com/oauth/authorize?"));
    drop(urls);

    let recreated_view = handle.subscribe();
    let reopened = recreated_view.try_recv().unwrap();
    assert!(matches!(reopened.account, AccountState::SignedIn { .. }));
    assert!(matches!(reopened.model_list, ModelListState::Loaded(_)));

    let refresh_id = handle
        .try_submit(AiCommand::RefreshAccount {
            refresh_token: false,
        })
        .unwrap();
    assert_eq!(
        wait_for_result(&service, refresh_id).0,
        SafeOperationResult::Succeeded
    );
    assert!(matches!(
        handle.snapshot().account,
        AccountState::SignedIn { .. }
    ));

    let logout_id = handle.try_submit(AiCommand::Logout).unwrap();
    assert_eq!(
        wait_for_result(&service, logout_id).0,
        SafeOperationResult::Succeeded
    );
    assert_eq!(handle.snapshot().account, AccountState::SignedOut);
    assert!(
        !data_root
            .join("ai/codex-chatgpt/.hane-fake-app-server-signed-in")
            .exists(),
        "logout must clear the fake App Server's persisted account state"
    );

    let before_key = handle.snapshot().settings;
    let key_save_id = handle
        .try_update_custom_credential(
            before_key.revision,
            None,
            before_key.clone(),
            "fixture-only-key".to_string(),
        )
        .unwrap();
    assert_eq!(
        wait_for_result(&service, key_save_id).0,
        SafeOperationResult::Succeeded
    );
    let after_key = handle.snapshot().settings;
    let credential_ref = after_key
        .custom
        .as_ref()
        .unwrap()
        .credential_ref
        .clone()
        .unwrap();
    assert_eq!(
        credential_store.get(&credential_ref).unwrap().as_deref(),
        Some("fixture-only-key")
    );
    let settings_file = std::fs::read_to_string(data_root.join("ai/ai-settings.json")).unwrap();
    assert!(!settings_file.contains("fixture-only-key"));

    let mut custom_active = after_key.clone();
    custom_active.active_connection = ActiveConnection::Custom;
    let switch_id = handle
        .try_save_settings(after_key.revision, custom_active)
        .unwrap();
    assert_eq!(
        wait_for_result(&service, switch_id).0,
        SafeOperationResult::Succeeded
    );
    assert_eq!(
        handle.snapshot().settings.active_connection,
        ActiveConnection::Custom
    );
    assert_eq!(
        handle.snapshot().runtime_state,
        hane_ai::RuntimeState::Ready
    );

    let mut without_key = handle.snapshot().settings;
    without_key.custom.as_mut().unwrap().credential_ref = None;
    let delete_id = handle
        .try_delete_custom_credential(without_key.revision, credential_ref, without_key)
        .unwrap();
    assert_eq!(
        wait_for_result(&service, delete_id).0,
        SafeOperationResult::Failed("apply_failed")
    );
    let deleted = handle.snapshot();
    assert_eq!(deleted.settings.active_connection, ActiveConnection::Custom);
    assert_eq!(
        deleted.settings.custom.as_ref().unwrap().credential_ref,
        None
    );
    assert_eq!(deleted.runtime_state, hane_ai::RuntimeState::Stopped);
    assert_eq!(deleted.persistence, PersistenceState::Saved);
    assert!(credential_store.is_empty());
}

#[test]
fn cancellation_reconciles_a_completion_race_using_account_read() {
    for (scenario, expected, signed_in) in [
        ("hold_login", SafeOperationResult::Canceled, false),
        ("complete_on_cancel", SafeOperationResult::Succeeded, true),
    ] {
        let data_root = unique_data_root();
        configure_fake_scenario(&data_root, scenario);
        let service = spawn_service(data_root, Arc::new(RecordingBrowser::default()));
        open_settings(&service);

        let login_id = service.handle().try_submit(AiCommand::StartLogin).unwrap();
        wait_until_login_is_waiting(&service);
        service.handle().cancel(login_id).unwrap();

        let (result, login) = wait_for_result(&service, login_id);
        assert_eq!(result, expected, "scenario {scenario}");
        assert_eq!(login, LoginState::Finished, "scenario {scenario}");
        assert_eq!(
            matches!(
                service.handle().snapshot().account,
                AccountState::SignedIn { .. }
            ),
            signed_in,
            "account/read is authoritative for scenario {scenario}"
        );
    }
}

#[test]
fn browser_open_failure_cancels_and_reconciles_without_inventing_login_success() {
    let data_root = unique_data_root();
    configure_fake_scenario(&data_root, "hold_login");
    let service = spawn_service(data_root, Arc::new(FailingBrowser));
    open_settings(&service);

    let login_id = service.handle().try_submit(AiCommand::StartLogin).unwrap();
    assert_eq!(
        wait_for_result(&service, login_id),
        (
            SafeOperationResult::Failed("browser_unavailable"),
            LoginState::Finished
        )
    );
    assert_eq!(service.handle().snapshot().account, AccountState::SignedOut);
}

#[test]
fn probe_requires_terminal_final_text_and_sends_only_the_fixed_request_profile() {
    let (service, data_root) = prepare_probe_service("probe_success");
    std::fs::write(
        data_root.join("open-document.md"),
        "DO_NOT_SEND_DOCUMENT_379",
    )
    .unwrap();
    let handle = service.handle();
    let probe_id = handle.try_submit(AiCommand::Probe).unwrap();
    assert_eq!(
        wait_for_result(&service, probe_id).0,
        SafeOperationResult::Succeeded
    );

    let snapshot = handle.snapshot();
    assert_eq!(snapshot.probe_status, ProbeStatus::Succeeded);
    let result = snapshot.probe_result.as_ref().unwrap();
    assert_eq!(
        result.text, "Mock reply",
        "duplicate item and foreign-turn text must be ignored"
    );
    assert_eq!(result.connection, ActiveConnection::ChatGpt);
    assert_eq!(result.model, "gpt-fake-text");
    assert_eq!(
        result.settings_generation,
        snapshot.settings.settings_generation
    );
    assert_eq!(result.runtime_generation, snapshot.runtime_generation);

    let record = wait_for_probe_record(&data_root, "TURN_START:");
    assert!(record.contains("Reply with exactly HANE_AI_OK."));
    assert!(record.contains("\"modelProvider\":\"openai\""));
    assert!(record.contains("\"ephemeral\":true"));
    assert!(record.contains("\"sandbox\":\"read-only\""));
    assert!(record.contains("\"approvalPolicy\":\"never\""));
    assert!(record.contains("\"baseInstructions\""));
    assert!(record.contains("\"developerInstructions\""));
    assert!(!record.contains("DO_NOT_SEND_DOCUMENT_379"));
    assert!(!format!("{snapshot:?}").contains("sk-fake-secret"));
    let probe_cwd = record
        .lines()
        .find_map(|line| line.strip_prefix("THREAD_START:"))
        .and_then(|params| serde_json::from_str::<serde_json::Value>(params).ok())
        .and_then(|params| params["cwd"].as_str().map(std::path::PathBuf::from))
        .expect("thread/start records its dedicated workspace");
    assert!(
        !probe_cwd.exists(),
        "a terminal probe removes its dedicated workspace"
    );
    drop(service);
    let _ = std::fs::remove_dir_all(data_root);
}

#[test]
fn probe_keeps_settings_and_service_operations_busy_until_terminal() {
    let (service, data_root) = prepare_probe_service("probe_wait_terminal");
    let handle = service.handle();
    let probe_id = handle.try_submit(AiCommand::Probe).unwrap();
    wait_for_probe_record(&data_root, "TURN_START:");
    assert_eq!(
        handle.snapshot().busy,
        Some((probe_id, ServiceBusyReason::Probe))
    );
    assert_eq!(
        handle.try_submit(AiCommand::Logout),
        Err(AdmissionError::Busy)
    );
    assert_eq!(
        handle.try_submit(AiCommand::Restart),
        Err(AdmissionError::Busy)
    );
    let settings = handle.snapshot().settings;
    assert_eq!(
        handle.try_save_settings(settings.revision, settings),
        Err(AdmissionError::Busy)
    );
    let settings_lock = AiSettingsLock::new(data_root.join("ai/ai-settings.lock"));
    assert!(settings_lock.try_acquire_exclusive().unwrap().is_none());

    handle.cancel(probe_id).unwrap();
    assert_eq!(
        wait_for_result(&service, probe_id).0,
        SafeOperationResult::Canceled
    );
    assert_eq!(handle.snapshot().probe_status, ProbeStatus::Canceled);
    assert!(settings_lock.try_acquire_exclusive().unwrap().is_some());
    drop(service);
    let _ = std::fs::remove_dir_all(data_root);
}

#[test]
fn interrupt_ack_alone_does_not_release_probe_guard_before_child_stops() {
    let (service, data_root) = prepare_probe_service("probe_cancel_ack_only");
    let handle = service.handle();
    let probe_id = handle.try_submit(AiCommand::Probe).unwrap();
    wait_for_probe_record(&data_root, "TURN_START:");
    handle.cancel(probe_id).unwrap();
    wait_for_probe_record(&data_root, "INTERRUPT_ACK");

    assert_eq!(
        handle.snapshot().busy,
        Some((probe_id, ServiceBusyReason::Probe)),
        "interrupt RPC acknowledgement is not a terminal turn result"
    );
    let settings_lock = AiSettingsLock::new(data_root.join("ai/ai-settings.lock"));
    assert!(settings_lock.try_acquire_exclusive().unwrap().is_none());

    assert_eq!(
        wait_for_result(&service, probe_id).0,
        SafeOperationResult::Canceled
    );
    assert_eq!(
        handle.snapshot().runtime_state,
        hane_ai::RuntimeState::Stopped
    );
    assert!(settings_lock.try_acquire_exclusive().unwrap().is_some());
    drop(service);
    let _ = std::fs::remove_dir_all(data_root);
}

#[test]
fn probe_tool_item_start_is_not_reported_as_success() {
    let (service, data_root) = prepare_probe_service("probe_tool_item");
    let handle = service.handle();
    let probe_id = handle.try_submit(AiCommand::Probe).unwrap();
    assert_eq!(
        wait_for_result(&service, probe_id).0,
        SafeOperationResult::Failed(ProbeErrorCode::SafetyProfileUnsupported.stable_code())
    );
    let snapshot = handle.snapshot();
    assert_eq!(
        snapshot.probe_status,
        ProbeStatus::Failed(ProbeErrorCode::SafetyProfileUnsupported)
    );
    assert_eq!(snapshot.probe_result, None);
    assert!(wait_for_probe_record(&data_root, "INTERRUPT_ACK").contains("INTERRUPT_ACK"));
    drop(service);
    let _ = std::fs::remove_dir_all(data_root);
}

#[test]
fn probe_provider_errors_are_classified_without_switching_or_exposing_error_text() {
    for (scenario, code) in [
        ("probe_401", ProbeErrorCode::Unauthorized),
        ("probe_403", ProbeErrorCode::Forbidden),
        ("probe_429", ProbeErrorCode::RateLimited),
    ] {
        let (service, data_root) = prepare_probe_service(scenario);
        let handle = service.handle();
        let probe_id = handle.try_submit(AiCommand::Probe).unwrap();
        assert_eq!(
            wait_for_result(&service, probe_id).0,
            SafeOperationResult::Failed(code.stable_code())
        );
        let snapshot = handle.snapshot();
        assert_eq!(snapshot.probe_status, ProbeStatus::Failed(code));
        assert_eq!(
            snapshot.settings.active_connection,
            ActiveConnection::ChatGpt
        );
        assert!(!format!("{snapshot:?}").contains("sk-fake-secret"));
        drop(service);
        let _ = std::fs::remove_dir_all(data_root);
    }
}

#[test]
fn probe_timeout_and_invalid_turn_ack_fail_safely_after_confirmed_stop() {
    for (scenario, expected_result, expected_status) in [
        (
            "probe_turn_timeout",
            SafeOperationResult::TimedOut,
            ProbeStatus::TimedOut,
        ),
        (
            "probe_invalid_turn_ack",
            SafeOperationResult::Failed(ProbeErrorCode::ProtocolMismatch.stable_code()),
            ProbeStatus::Failed(ProbeErrorCode::ProtocolMismatch),
        ),
    ] {
        let (service, data_root) = prepare_probe_service(scenario);
        let handle = service.handle();
        let probe_id = handle.try_submit(AiCommand::Probe).unwrap();
        assert_eq!(
            wait_for_result(&service, probe_id).0,
            expected_result,
            "scenario {scenario}"
        );
        let snapshot = handle.snapshot();
        assert_eq!(snapshot.probe_status, expected_status);
        assert_eq!(snapshot.runtime_state, hane_ai::RuntimeState::Stopped);
        assert_eq!(
            snapshot.settings.active_connection,
            ActiveConnection::ChatGpt
        );
        drop(service);
        let _ = std::fs::remove_dir_all(data_root);
    }
}

#[test]
fn nonowner_probe_is_rejected_until_owner_exits_then_settings_are_reloaded() {
    let data_root = unique_data_root();
    let first = spawn_service(data_root.clone(), Arc::new(RecordingBrowser::default()));
    open_settings(&first);
    let first_handle = first.handle();
    let mut settings = first_handle.snapshot().settings;
    settings.chatgpt.model_id = Some("gpt-fake-text".to_owned());
    let save_id = first_handle
        .try_save_settings(settings.revision, settings)
        .unwrap();
    assert_eq!(
        wait_for_result(&first, save_id).0,
        SafeOperationResult::Succeeded
    );
    let saved_generation = first_handle.snapshot().settings.settings_generation;

    let second = spawn_service(data_root.clone(), Arc::new(RecordingBrowser::default()));
    let second_handle = second.handle();
    let open_id = second_handle.try_submit(AiCommand::OpenSettings).unwrap();
    assert_eq!(
        wait_for_result(&second, open_id).0,
        SafeOperationResult::Failed("owned_elsewhere")
    );
    assert_eq!(
        second_handle.snapshot().ownership,
        hane_ai::OwnershipState::OwnedElsewhere
    );
    assert_eq!(
        second_handle.try_submit(AiCommand::Probe),
        Err(AdmissionError::Unavailable)
    );

    drop(first);
    let retry_id = second_handle.try_submit(AiCommand::OpenSettings).unwrap();
    assert_eq!(
        wait_for_result(&second, retry_id).0,
        SafeOperationResult::Succeeded
    );
    assert_eq!(
        second_handle.snapshot().ownership,
        hane_ai::OwnershipState::Owned
    );
    assert_eq!(
        second_handle.snapshot().settings.settings_generation,
        saved_generation
    );
    assert_eq!(
        second_handle
            .snapshot()
            .settings
            .chatgpt
            .model_id
            .as_deref(),
        Some("gpt-fake-text")
    );
    drop(second);
    let _ = std::fs::remove_dir_all(data_root);
}

#[test]
fn a_new_service_reads_persisted_account_state_after_runtime_restart() {
    let data_root = unique_data_root();
    let first_service = spawn_service(data_root.clone(), Arc::new(RecordingBrowser::default()));
    open_settings(&first_service);
    let login_id = first_service
        .handle()
        .try_submit(AiCommand::StartLogin)
        .unwrap();
    assert_eq!(
        wait_for_result(&first_service, login_id).0,
        SafeOperationResult::Succeeded
    );
    assert!(
        data_root
            .join("ai/codex-chatgpt/.hane-fake-app-server-signed-in")
            .exists(),
        "fake account state was not persisted under CODEX_HOME: {data_root:?}; observed env: {:?}",
        observed_fake_codex_home(&data_root)
    );
    drop(first_service);

    let restarted = spawn_service(data_root, Arc::new(RecordingBrowser::default()));
    open_settings(&restarted);
    let refresh_id = restarted
        .handle()
        .try_submit(AiCommand::RefreshAccount {
            refresh_token: false,
        })
        .unwrap();
    assert_eq!(
        wait_for_result(&restarted, refresh_id).0,
        SafeOperationResult::Succeeded
    );
    assert!(
        matches!(
            restarted.handle().snapshot().account,
            AccountState::SignedIn { .. }
        ),
        "restarted account snapshot: {:?}",
        restarted.handle().snapshot()
    );
}

#[test]
fn failed_runtime_start_marks_ownership_unknown_and_allows_reopen() {
    let data_root = unique_data_root();
    configure_fake_scenario(&data_root, "crash_after_initialize");
    let service = spawn_service(data_root.clone(), Arc::new(RecordingBrowser::default()));
    open_settings(&service);

    let handle = service.handle();
    let login_id = handle.try_submit(AiCommand::StartLogin).unwrap();
    assert_eq!(
        wait_for_result(&service, login_id).0,
        SafeOperationResult::Failed("runtime_unavailable")
    );
    assert_eq!(handle.snapshot().ownership, OwnershipState::Unknown);

    let owner_lock = OwnerLock::new(data_root.join("ai/runtime-owner.lock"));
    let release_deadline = Instant::now() + Duration::from_secs(2);
    let acquired = loop {
        if let Some(acquired) = owner_lock.try_acquire().unwrap() {
            break acquired;
        }
        assert!(
            Instant::now() < release_deadline,
            "failed startup must eventually release the lock instead of remaining Owned"
        );
        std::thread::sleep(Duration::from_millis(10));
    };
    drop(acquired);

    open_settings(&service);
    assert_eq!(handle.snapshot().ownership, OwnershipState::Owned);
    drop(service);
    let _ = std::fs::remove_dir_all(data_root);
}
