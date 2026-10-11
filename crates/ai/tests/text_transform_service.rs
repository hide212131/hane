//! Integration coverage for the selection AI text transform operation
//! (Issue #449), against the same fake App Server fixture the Probe
//! integration tests use.
//!
//! Per the PR #450 review finding this fixes (ADR-0032 section 10):
//! `sandbox: "read-only"` / `approvalPolicy: "never"` do not by themselves
//! prove that a text transform request cannot read outside
//! `instruction`/`selected_text`, write, execute a shell/tool, or reach the
//! network, and declining an approval request after it is raised cannot
//! undo an earlier read/send. Until an officially supported App
//! Server/runtime configuration verifiably guarantees that boundary before
//! a request starts, `hane_ai::text_transform::execute_text_transform`
//! fails closed before ever calling `thread/start` — including for this
//! fixture's synthetic scenarios, which is why the scenario-specific
//! round-trip coverage this file previously had (a successful turn, an
//! in-flight interrupt-ack, mid-turn tool-item detection) no longer applies
//! to text transform: none of those turns can start at all right now. The
//! shared detection/interrupt logic those scenarios exercise is still
//! covered via the fixed Probe's own integration tests in
//! `account_service.rs` (`probe_tool_item_start_is_not_reported_as_success`,
//! `interrupt_ack_alone_does_not_release_probe_guard_before_child_stops`),
//! since `text_transform.rs` shares that code with `probe.rs`.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use hane_ai::{
    AccountState, AdmissionError, AiCommand, AiService, AiServiceConfig,
    BrowserOpenError, BrowserOpener, FakeCredentialStore, ModelListState,
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

fn find_turn_start_with_selected_text(
    record: &str,
    selected_text: &str,
) -> Option<serde_json::Value> {
    record
        .lines()
        .filter_map(|line| line.strip_prefix("TURN_START:"))
        .find_map(|params| {
            let turn_start: serde_json::Value = serde_json::from_str(params).ok()?;
            let input_text = turn_start["input"].get(0)?.get("text")?.as_str()?;
            let payload: serde_json::Value = serde_json::from_str(input_text).ok()?;
            (payload["selected_text"].as_str() == Some(selected_text)).then_some(turn_start)
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
fn text_transform_fails_closed_before_any_app_server_request() {
    let (service, data_root) = prepare_connected_service("probe_success");
    std::fs::write(
        data_root.join("open-document.md"),
        "DO_NOT_SEND_DOCUMENT_449",
    )
    .unwrap();
    let handle = service.handle();
    let selected_text = "選択された文章";
    let (id, reply_rx) = handle
        .try_submit_text_transform("校正する".to_owned(), selected_text.to_owned())
        .unwrap();
    assert_eq!(
        wait_for_result(&service, id),
        SafeOperationResult::Failed(
            TextTransformErrorCode::IsolationBoundaryUnverified.stable_code()
        )
    );

    let outcome = reply_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("the request's own reply channel delivers exactly one outcome");
    assert_eq!(
        outcome,
        TextTransformOutcome::Failed(TextTransformErrorCode::IsolationBoundaryUnverified)
    );

    // Neither the selected text nor the open document's content ever
    // reached the App Server: the shared probe-record file (populated by
    // the account/model setup above) carries no turn/start for this
    // selected_text, and no trace of the document at all.
    let record = std::fs::read_to_string(
        data_root.join("ai/codex-chatgpt/.hane-fake-app-server-probe-record"),
    )
    .unwrap_or_default();
    assert!(find_turn_start_with_selected_text(&record, selected_text).is_none());
    assert!(!record.contains("DO_NOT_SEND_DOCUMENT_449"));
    assert!(!record.contains(selected_text));

    // The snapshot never carries this operation's text either.
    let snapshot = handle.snapshot();
    assert!(!format!("{snapshot:?}").contains(selected_text));

    drop(service);
    let _ = std::fs::remove_dir_all(data_root);
}

#[test]
fn text_transform_rejects_invalid_input_before_touching_busy_state() {
    let (service, data_root) = prepare_connected_service("probe_success");
    let handle = service.handle();

    assert!(matches!(
        handle.try_submit_text_transform(String::new(), "text".to_owned()),
        Err(AdmissionError::InvalidInput)
    ));
    assert!(matches!(
        handle.try_submit_text_transform("instruction".to_owned(), String::new()),
        Err(AdmissionError::InvalidInput)
    ));
    assert!(handle.snapshot().busy.is_none());

    drop(service);
    let _ = std::fs::remove_dir_all(data_root);
}

#[test]
fn text_transform_is_rejected_as_busy_while_a_probe_is_in_flight_and_still_fails_closed_after()
 {
    let (service, data_root) = prepare_connected_service("probe_wait_terminal");
    let handle = service.handle();
    let probe_id = handle.try_submit(AiCommand::Probe).unwrap();
    wait_for_probe_record(&data_root, "TURN_START:");
    assert!(matches!(
        handle.try_submit_text_transform("校正する".to_owned(), "本文".to_owned()),
        Err(AdmissionError::Busy)
    ));
    assert_eq!(
        handle.snapshot().busy,
        Some((probe_id, ServiceBusyReason::Probe))
    );
    handle.cancel(probe_id).unwrap();
    assert_eq!(wait_for_result(&service, probe_id), SafeOperationResult::Canceled);

    // Once the Probe has released the guard, a text transform is admitted
    // again, but — unlike the fixed Probe — it still fails closed before
    // ever reaching the App Server.
    let (id, reply_rx) = handle
        .try_submit_text_transform("校正する".to_owned(), "選択テキスト".to_owned())
        .unwrap();
    assert_eq!(
        wait_for_result(&service, id),
        SafeOperationResult::Failed(
            TextTransformErrorCode::IsolationBoundaryUnverified.stable_code()
        )
    );
    assert_eq!(
        reply_rx.recv_timeout(Duration::from_secs(5)).unwrap(),
        TextTransformOutcome::Failed(TextTransformErrorCode::IsolationBoundaryUnverified)
    );

    drop(service);
    let _ = std::fs::remove_dir_all(data_root);
}
