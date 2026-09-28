use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use hane_ai::{
    AccountState, ActiveConnection, AiCommand, AiService, AiServiceConfig, BrowserOpenError,
    BrowserOpener, CredentialStore, CustomConnectionSettings, FakeCredentialStore, LoginState,
    ModelListState, OperationId, PersistenceState, SafeOperationResult,
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
    assert_eq!(
        observed_fake_codex_home(&data_root).as_deref(),
        data_root.join("ai/codex-chatgpt").to_str(),
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
