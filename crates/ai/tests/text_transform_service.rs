//! Integration coverage for the selection AI text transform operation
//! (Issue #449), against the same fake App Server fixture the Probe
//! integration tests use. Scenario strings (`probe_success`,
//! `probe_tool_item`, `probe_cancel_ack_only`, ...) are generic at the
//! `turn/start` level and are reused unmodified: the fixture does not
//! distinguish which AI crate capability issued the request.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use hane_ai::{
    AccountState, ActiveConnection, AdmissionError, AiCommand, AiService, AiServiceConfig,
    AiSettingsLock, BrowserOpenError, BrowserOpener, FakeCredentialStore, ModelListState,
    OperationId, SafeOperationResult, ServiceBusyReason, ShellEnvironmentPolicyFormat,
    TextTransformErrorCode, TextTransformOutcome,
};

#[derive(Default)]
struct RecordingBrowser(Mutex<Vec<String>>);

impl BrowserOpener for RecordingBrowser {
    fn open(&self, url: &str) -> Result<(), BrowserOpenError> {
        self.0.lock().unwrap().push(url.to_owned());
        Ok(())
    }
}

fn wait_for_result(service: &AiService, id: OperationId) -> SafeOperationResult {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let snapshot = service.handle().snapshot();
        if let Some((finished_id, result)) = &snapshot.last_result
            && *finished_id == id
            && snapshot.busy.is_none()
        {
            return result.clone();
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
        "hane-ai-text-transform-service-{}-{id}",
        std::process::id()
    ))
}

fn configure_fake_scenario(root: &std::path::Path, scenario: &str) {
    let codex_home = root.join("ai/codex-chatgpt");
    std::fs::create_dir_all(&codex_home).unwrap();
    std::fs::write(codex_home.join(".hane-fake-app-server-scenario"), scenario).unwrap();
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
            "fixture did not record {needle:?}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
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
    let id = service.handle().try_submit(AiCommand::OpenSettings).unwrap();
    assert_eq!(wait_for_result(service, id), SafeOperationResult::Succeeded);
}

/// Spawns a service already signed in, with a model saved, against a fake
/// App Server configured for `scenario`.
fn prepare_connected_service(scenario: &str) -> (AiService, PathBuf) {
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
    assert_eq!(wait_for_result(&service, save_id), SafeOperationResult::Succeeded);

    let account_id = handle
        .try_submit(AiCommand::RefreshAccount {
            refresh_token: false,
        })
        .unwrap();
    assert_eq!(wait_for_result(&service, account_id), SafeOperationResult::Succeeded);
    assert!(matches!(
        handle.snapshot().account,
        AccountState::SignedIn { .. }
    ));

    let models_id = handle.try_submit(AiCommand::RefreshModels).unwrap();
    assert_eq!(wait_for_result(&service, models_id), SafeOperationResult::Succeeded);
    assert!(matches!(
        handle.snapshot().model_list,
        ModelListState::Loaded(_)
    ));
    (service, data_root)
}

#[test]
fn text_transform_sends_only_the_instruction_and_selected_text_and_succeeds() {
    let (service, data_root) = prepare_connected_service("probe_success");
    std::fs::write(
        data_root.join("open-document.md"),
        "DO_NOT_SEND_DOCUMENT_449",
    )
    .unwrap();
    let handle = service.handle();
    let (id, reply_rx) = handle
        .try_submit_text_transform(
            "校正する".to_owned(),
            "選択された文章".to_owned(),
        )
        .unwrap();
    assert_eq!(wait_for_result(&service, id), SafeOperationResult::Succeeded);

    let outcome = reply_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("the request's own reply channel delivers exactly one outcome");
    assert_eq!(
        outcome,
        TextTransformOutcome::Succeeded {
            text: "Mock reply".to_owned()
        },
        "duplicate item and foreign-turn text must be ignored, same as the fixed Probe"
    );

    let record = wait_for_probe_record(&data_root, "TURN_START:");
    let turn_start: serde_json::Value = record
        .lines()
        .find_map(|line| line.strip_prefix("TURN_START:"))
        .and_then(|params| serde_json::from_str(params).ok())
        .expect("turn/start was recorded");
    let input_text = turn_start["input"][0]["text"]
        .as_str()
        .expect("turn/start sends one structured text input item");
    let payload: serde_json::Value = serde_json::from_str(input_text).unwrap();
    assert_eq!(payload["instruction"], "校正する");
    assert_eq!(payload["selected_text"], "選択された文章");
    assert!(!record.contains("DO_NOT_SEND_DOCUMENT_449"));
    // The thread/start cwd must be the dedicated text-transform workspace,
    // never the fixed Probe's own workspace directory.
    let cwd = record
        .lines()
        .find_map(|line| line.strip_prefix("THREAD_START:"))
        .and_then(|params| serde_json::from_str::<serde_json::Value>(params).ok())
        .and_then(|params| params["cwd"].as_str().map(PathBuf::from))
        .expect("thread/start records its dedicated workspace");
    assert!(cwd.starts_with(data_root.join("ai/text-transform-workspace")));
    assert!(
        !cwd.exists(),
        "a terminal text transform removes its dedicated workspace"
    );

    // The snapshot never carries this operation's text.
    let snapshot = handle.snapshot();
    assert!(!format!("{snapshot:?}").contains("選択された文章"));
    assert!(!format!("{snapshot:?}").contains("Mock reply"));

    drop(service);
    let _ = std::fs::remove_dir_all(data_root);
}

#[test]
fn text_transform_rejects_invalid_input_before_touching_busy_state() {
    let (service, data_root) = prepare_connected_service("probe_success");
    let handle = service.handle();

    assert_eq!(
        handle.try_submit_text_transform(String::new(), "text".to_owned()),
        Err(AdmissionError::InvalidInput)
    );
    assert_eq!(
        handle.try_submit_text_transform("instruction".to_owned(), String::new()),
        Err(AdmissionError::InvalidInput)
    );
    assert!(handle.snapshot().busy.is_none());

    drop(service);
    let _ = std::fs::remove_dir_all(data_root);
}

#[test]
fn text_transform_keeps_other_ai_operations_busy_until_terminal_and_rejects_a_second_request() {
    let (service, data_root) = prepare_connected_service("probe_wait_terminal");
    let handle = service.handle();
    let (id, reply_rx) = handle
        .try_submit_text_transform("校正する".to_owned(), "本文".to_owned())
        .unwrap();
    wait_for_probe_record(&data_root, "TURN_START:");
    assert_eq!(
        handle.snapshot().busy,
        Some((id, ServiceBusyReason::TextTransform))
    );
    assert_eq!(
        handle.try_submit(AiCommand::Logout),
        Err(AdmissionError::Busy)
    );
    assert_eq!(
        handle.try_submit_text_transform("another".to_owned(), "本文2".to_owned()),
        Err(AdmissionError::Busy),
        "a second text transform must not run concurrently with the first"
    );
    let settings_lock = AiSettingsLock::new(data_root.join("ai/ai-settings.lock"));
    assert!(settings_lock.try_acquire_exclusive().unwrap().is_none());

    handle.cancel(id).unwrap();
    assert_eq!(wait_for_result(&service, id), SafeOperationResult::Canceled);
    assert_eq!(
        reply_rx.recv_timeout(Duration::from_secs(5)).unwrap(),
        TextTransformOutcome::Canceled
    );
    assert!(settings_lock.try_acquire_exclusive().unwrap().is_some());

    drop(service);
    let _ = std::fs::remove_dir_all(data_root);
}

#[test]
fn interrupt_ack_alone_does_not_release_the_text_transform_guard_before_the_child_stops() {
    let (service, data_root) = prepare_connected_service("probe_cancel_ack_only");
    let handle = service.handle();
    let (id, reply_rx) = handle
        .try_submit_text_transform("校正する".to_owned(), "本文".to_owned())
        .unwrap();
    wait_for_probe_record(&data_root, "TURN_START:");
    handle.cancel(id).unwrap();
    wait_for_probe_record(&data_root, "INTERRUPT_ACK");

    assert_eq!(
        handle.snapshot().busy,
        Some((id, ServiceBusyReason::TextTransform)),
        "an interrupt RPC acknowledgement alone is not a terminal turn result"
    );
    let settings_lock = AiSettingsLock::new(data_root.join("ai/ai-settings.lock"));
    assert!(settings_lock.try_acquire_exclusive().unwrap().is_none());

    assert_eq!(wait_for_result(&service, id), SafeOperationResult::Canceled);
    assert_eq!(
        reply_rx.recv_timeout(Duration::from_secs(5)).unwrap(),
        TextTransformOutcome::Canceled
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
fn text_transform_tool_activity_is_never_reported_as_success() {
    let (service, data_root) = prepare_connected_service("probe_tool_item");
    let handle = service.handle();
    let (id, reply_rx) = handle
        .try_submit_text_transform("校正する".to_owned(), "本文".to_owned())
        .unwrap();
    assert_eq!(
        wait_for_result(&service, id),
        SafeOperationResult::Failed(TextTransformErrorCode::SafetyProfileUnsupported.stable_code())
    );
    assert_eq!(
        reply_rx.recv_timeout(Duration::from_secs(5)).unwrap(),
        TextTransformOutcome::Failed(TextTransformErrorCode::SafetyProfileUnsupported)
    );

    drop(service);
    let _ = std::fs::remove_dir_all(data_root);
}

#[test]
fn a_fixed_probe_and_a_text_transform_never_run_concurrently_and_probe_input_never_carries_selection_text()
 {
    let (service, data_root) = prepare_connected_service("probe_wait_terminal");
    let handle = service.handle();
    let probe_id = handle.try_submit(AiCommand::Probe).unwrap();
    wait_for_probe_record(&data_root, "TURN_START:");
    assert_eq!(
        handle.try_submit_text_transform("校正する".to_owned(), "本文".to_owned()),
        Err(AdmissionError::Busy)
    );
    handle.cancel(probe_id).unwrap();
    assert_eq!(wait_for_result(&service, probe_id), SafeOperationResult::Canceled);

    // Once the Probe has released the guard, a text transform is admitted
    // and sends its own structured instruction/selected_text (never the
    // fixed Probe's own fixed request text).
    let (id, reply_rx) = handle
        .try_submit_text_transform("校正する".to_owned(), "選択テキスト".to_owned())
        .unwrap();
    wait_for_probe_record(&data_root, "TURN_START:");
    handle.cancel(id).unwrap();
    assert_eq!(wait_for_result(&service, id), SafeOperationResult::Canceled);
    let outcome = reply_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(outcome, TextTransformOutcome::Canceled);
    let record = std::fs::read_to_string(
        data_root.join("ai/codex-chatgpt/.hane-fake-app-server-probe-record"),
    )
    .unwrap();
    let text_transform_turn_start: serde_json::Value = record
        .lines()
        .filter_map(|line| line.strip_prefix("TURN_START:"))
        .last()
        .and_then(|params| serde_json::from_str(params).ok())
        .expect("the text transform's own TURN_START was recorded");
    let input_text = text_transform_turn_start["input"][0]["text"]
        .as_str()
        .expect("turn/start sends one structured text input item");
    assert!(
        !input_text.contains("Reply with exactly HANE_AI_OK."),
        "the fixed Probe's own fixed input must never be the text transform's input: {input_text}"
    );
    let payload: serde_json::Value = serde_json::from_str(input_text).unwrap();
    assert_eq!(payload["selected_text"], "選択テキスト");

    drop(service);
    let _ = std::fs::remove_dir_all(data_root);
}
