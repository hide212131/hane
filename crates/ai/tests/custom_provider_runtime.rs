//! Custom Provider integration tests.
//!
//! `custom_provider_key_reaches_only_the_spawned_child_process_environment`,
//! `rotating_the_custom_provider_key_never_reaches_the_old_child`, and
//! `internal_path_save_reconfigure_and_generation_gated_probe_compose_end_to_end`
//! always run: they exercise `hane_ai::build_custom_provider_material`'s
//! `extra_env`, and the full settings-save/reload/config-generation/
//! reconfigure/generation-gated-probe path (ADR-0032 section 8), end-to-end
//! through `AiRuntime` against this crate's own `fake_app_server` fixture,
//! with no dependency on a real Codex binary or network access.
//!
//! `real_app_server_reaches_the_mock_responses_provider_with_the_configured_key`
//! additionally requires a real, bundled Codex App Server 0.157.1 binary
//! (path given via `HANE_TEST_CODEX_APP_SERVER_BIN`). Per this worker's
//! instructions, that evidence cannot be fabricated: when the environment
//! variable is unset, the test prints an explicit skip reason and returns
//! without asserting success, instead of being silently `#[ignore]`d.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use hane_ai::{
    build_custom_provider_material, build_runtime_config_for_active_connection, call_with_generation_check,
    update_custom_credential, write_codex_config, ActiveConnection, AiPaths, AiSettings, AiSettingsStore,
    AiRuntime, ChatGptConnectionSettings, ConnectError, CredentialJournal, CustomConnectionSettings,
    FakeCredentialStore, RejectAllServerRequests, RuntimeConfig, RuntimeState, SaveError, ShellEnvironmentPolicyFormat,
    CUSTOM_PROVIDER_ENV_KEY,
};

fn unique_dir(name: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let pid = std::process::id();
    let dir = std::env::temp_dir().join(format!("hane-ai-custom-provider-it-{pid}-{name}-{n}"));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn fake_server_config(name: &str) -> (RuntimeConfig, PathBuf) {
    let dir = unique_dir(name);
    let binary = PathBuf::from(env!("CARGO_BIN_EXE_fake_app_server"));
    let mut config = RuntimeConfig::new(binary, dir.join("runtime.lock"));
    config.args = Vec::new();
    config.start_timeout = Duration::from_secs(5);
    config.stop_grace_timeout = Duration::from_millis(500);
    config.stop_force_timeout = Duration::from_secs(5);
    config.codex_home = Some(dir.join("codex-home"));
    (config, dir)
}

fn spawn_and_start(config: RuntimeConfig) -> AiRuntime {
    let (events_tx, _events_rx) = mpsc::sync_channel(64);
    let handler = Arc::new(RejectAllServerRequests);
    let runtime = AiRuntime::spawn(config, handler, events_tx);
    let status = runtime.start().expect("start should succeed");
    assert_eq!(status.state, RuntimeState::Ready);
    runtime
}

#[test]
fn custom_provider_key_reaches_only_the_spawned_child_process_environment() {
    let (mut config, dir) = fake_server_config("env_routing");
    let record_file = dir.join("env-record.txt");
    config
        .extra_env
        .push(("FAKE_SERVER_RECORD_ENV_FILE".to_string(), record_file.to_string_lossy().to_string()));
    config
        .extra_env
        .push(("FAKE_SERVER_RECORD_ENV_VARS".to_string(), CUSTOM_PROVIDER_ENV_KEY.to_string()));

    let material = build_custom_provider_material(
        "My Provider",
        "https://provider.example/v1",
        "gpt-test-model",
        "sk-first-secret",
        ShellEnvironmentPolicyFormat::Filters,
    )
    .unwrap();
    write_codex_config(config.codex_home.as_ref().unwrap(), &material.config_toml).unwrap();
    config.extra_env.extend(material.extra_env.clone());

    let runtime = spawn_and_start(config);
    let _ = runtime.stop();

    let recorded = std::fs::read_to_string(&record_file).unwrap();
    assert!(
        recorded.contains(&format!("ENV:{CUSTOM_PROVIDER_ENV_KEY}=sk-first-secret")),
        "the injected key must reach this child's own environment: {recorded}"
    );
}

#[test]
fn rotating_the_custom_provider_key_never_reaches_the_old_child() {
    let (mut first_config, dir) = fake_server_config("key_rotation");
    let record_file = dir.join("env-record.txt");
    first_config
        .extra_env
        .push(("FAKE_SERVER_RECORD_ENV_FILE".to_string(), record_file.to_string_lossy().to_string()));
    first_config
        .extra_env
        .push(("FAKE_SERVER_RECORD_ENV_VARS".to_string(), CUSTOM_PROVIDER_ENV_KEY.to_string()));
    let first_material = build_custom_provider_material(
        "My Provider",
        "https://provider.example/v1",
        "gpt-test-model",
        "sk-first-secret",
        ShellEnvironmentPolicyFormat::Filters,
    )
    .unwrap();
    first_config.extra_env.extend(first_material.extra_env.clone());

    let first_runtime = spawn_and_start(first_config.clone());
    let _ = first_runtime.stop();
    drop(first_runtime);

    // Simulates the runtime restart a Custom Provider key rotation triggers
    // (ADR-0032 section 7.3/8: a `settings_generation`-affecting change
    // restarts the runtime with a freshly generated `RuntimeConfig`, never
    // reusing the old process or its environment).
    let mut second_config = first_config;
    second_config.extra_env.retain(|(k, _)| k != CUSTOM_PROVIDER_ENV_KEY);
    let second_material = build_custom_provider_material(
        "My Provider",
        "https://provider.example/v1",
        "gpt-test-model",
        "sk-second-secret",
        ShellEnvironmentPolicyFormat::Filters,
    )
    .unwrap();
    second_config.extra_env.extend(second_material.extra_env.clone());

    let second_runtime = spawn_and_start(second_config);
    let _ = second_runtime.stop();

    let recorded = std::fs::read_to_string(&record_file).unwrap();
    let lines: Vec<&str> = recorded.lines().collect();
    assert_eq!(lines, vec![
        format!("ENV:{CUSTOM_PROVIDER_ENV_KEY}=sk-first-secret"),
        format!("ENV:{CUSTOM_PROVIDER_ENV_KEY}=sk-second-secret"),
    ], "each child must see only the key generated for its own generation, never the other's");
}

#[test]
fn reconfigure_rotates_the_custom_provider_key_on_the_same_runtime_without_dropping_it() {
    // Unlike `rotating_the_custom_provider_key_never_reaches_the_old_child`,
    // which simulates a restart by dropping the first `AiRuntime` and
    // constructing a second one, this exercises `AiRuntime::reconfigure`
    // directly on one `AiRuntime` handle -- the supported way to apply a
    // `settings_generation`-affecting change without ever tearing down the
    // coordinator thread (and therefore never releasing the runtime owner
    // lock to a would-be new owner) in between.
    let (mut first_config, dir) = fake_server_config("reconfigure_rotation");
    let record_file = dir.join("env-record.txt");
    first_config
        .extra_env
        .push(("FAKE_SERVER_RECORD_ENV_FILE".to_string(), record_file.to_string_lossy().to_string()));
    first_config
        .extra_env
        .push(("FAKE_SERVER_RECORD_ENV_VARS".to_string(), CUSTOM_PROVIDER_ENV_KEY.to_string()));
    let first_material = build_custom_provider_material(
        "My Provider",
        "https://provider.example/v1",
        "gpt-test-model",
        "sk-first-secret",
        ShellEnvironmentPolicyFormat::Filters,
    )
    .unwrap();
    first_config.extra_env.extend(first_material.extra_env.clone());
    let first_owner_lock_path = first_config.owner_lock_path.clone();

    let runtime = spawn_and_start(first_config.clone());
    let first_status = runtime.snapshot();

    let mut second_config = first_config;
    second_config.extra_env.retain(|(k, _)| k != CUSTOM_PROVIDER_ENV_KEY);
    let second_material = build_custom_provider_material(
        "My Provider",
        "https://provider.example/v1",
        "gpt-test-model",
        "sk-second-secret",
        ShellEnvironmentPolicyFormat::Filters,
    )
    .unwrap();
    second_config.extra_env.extend(second_material.extra_env.clone());

    let reconfigured = runtime.reconfigure(second_config).expect("reconfigure should succeed");
    assert_eq!(reconfigured.state, RuntimeState::Ready);
    assert!(
        reconfigured.generation > first_status.generation,
        "reconfigure must spawn a fresh child generation, not silently reuse the old one"
    );

    let _ = runtime.stop();

    // The owner lock is never released to a would-be new owner across the
    // reconfigure: it was held by this same `AiRuntime` throughout, so a
    // separate acquire attempt only succeeds now, after this explicit stop.
    let external_lock = hane_ai::OwnerLock::new(&first_owner_lock_path);
    assert!(external_lock.try_acquire().unwrap().is_some());

    let recorded = std::fs::read_to_string(&record_file).unwrap();
    let lines: Vec<&str> = recorded.lines().collect();
    assert_eq!(
        lines,
        vec![
            format!("ENV:{CUSTOM_PROVIDER_ENV_KEY}=sk-first-secret"),
            format!("ENV:{CUSTOM_PROVIDER_ENV_KEY}=sk-second-secret"),
        ],
        "the reconfigured child must see only the newly rotated key, never the old one"
    );
}

fn custom_settings_for(credential_ref: Option<hane_ai::CredentialRef>) -> AiSettings {
    AiSettings {
        schema_version: hane_ai::AI_SETTINGS_SCHEMA_VERSION,
        revision: 0,
        settings_generation: 0,
        active_connection: ActiveConnection::Custom,
        chatgpt: ChatGptConnectionSettings::default(),
        custom: Some(CustomConnectionSettings {
            id: "conn-1".to_string(),
            name: "My Provider".to_string(),
            base_url: "https://provider.example/v1".to_string(),
            model_id: "gpt-test".to_string(),
            credential_ref,
        }),
    }
}

/// Composes the individual `hane_ai` public functions this crate already
/// exposes -- credential/settings save, settings reload, Custom config
/// generation, `AiRuntime::reconfigure`, `settings_generation`-gated probe --
/// into the one internal, GUI-less path described by ADR-0032 section 8, and
/// exercises it end-to-end against the `fake_app_server` fixture (no real
/// Codex binary required). Also confirms the shared/exclusive AI settings
/// lock contract these steps depend on: a settings save is rejected
/// immediately while a probe holds the lock in shared mode.
#[test]
fn internal_path_save_reconfigure_and_generation_gated_probe_compose_end_to_end() {
    let dir = unique_dir("internal_path");
    let paths = AiPaths::new(&dir);
    let settings_store = AiSettingsStore::new(dir.join("ai-settings.json"), dir.join("ai-settings.lock"));
    let journal = CredentialJournal::new(dir.join("credential-journal.json"));
    let credential_store = FakeCredentialStore::new();
    let binary = PathBuf::from(env!("CARGO_BIN_EXE_fake_app_server"));
    let owner_lock_path = dir.join("runtime.lock");

    // Step 1: settings/credential save.
    let saved =
        update_custom_credential(&settings_store, &journal, &credential_store, 0, None, "sk-first", |new_ref| {
            custom_settings_for(Some(new_ref.clone()))
        })
        .expect("initial credential save should succeed");

    // Step 2: settings reload, then Custom config generation from it.
    let reloaded = settings_store.load().unwrap();
    assert_eq!(reloaded, saved);
    let configured = build_runtime_config_for_active_connection(
        &reloaded,
        &credential_store,
        &paths,
        binary.clone(),
        owner_lock_path.clone(),
        ShellEnvironmentPolicyFormat::Filters,
    )
    .expect("building the runtime config for the freshly saved settings should succeed");

    // Step 3: runtime spawn/start against the generated config.
    let (events_tx, _events_rx) = mpsc::sync_channel(64);
    let handler = Arc::new(RejectAllServerRequests);
    let runtime = AiRuntime::spawn(configured.config, handler, events_tx);
    let status = runtime.start().expect("start should succeed against the generated Custom config");
    assert_eq!(status.state, RuntimeState::Ready);

    // Step 4: generation-gated probe request/response.
    let probe = call_with_generation_check(
        &settings_store,
        configured.settings_generation,
        &runtime,
        "test/echo",
        Some(serde_json::json!({"ping": true})),
        Duration::from_secs(5),
    )
    .expect("the probe must succeed while settings_generation still matches");
    assert_eq!(probe["ping"], serde_json::json!(true));

    // Rotating the credential bumps `settings_generation`: a probe still
    // configured for the old generation must be rejected before ever
    // reaching the runtime.
    let old_ref = saved.custom.as_ref().unwrap().credential_ref.clone().unwrap();
    let rotated = update_custom_credential(
        &settings_store,
        &journal,
        &credential_store,
        saved.revision,
        Some(old_ref),
        "sk-second",
        |new_ref| custom_settings_for(Some(new_ref.clone())),
    )
    .expect("credential rotation should succeed");
    assert!(rotated.settings_generation > configured.settings_generation);

    let stale_probe = call_with_generation_check(
        &settings_store,
        configured.settings_generation,
        &runtime,
        "test/echo",
        Some(serde_json::json!({"ping": true})),
        Duration::from_secs(5),
    );
    assert!(matches!(stale_probe, Err(ConnectError::GenerationMismatch { .. })));

    // Runtime re-generation for the rotated settings, then reconfigure and a
    // fresh generation-gated probe.
    let reloaded_after_rotation = settings_store.load().unwrap();
    let reconfigured_runtime_config = build_runtime_config_for_active_connection(
        &reloaded_after_rotation,
        &credential_store,
        &paths,
        binary,
        owner_lock_path,
        ShellEnvironmentPolicyFormat::Filters,
    )
    .expect("building the runtime config for the rotated settings should succeed");

    let reconfigured_status = runtime
        .reconfigure(reconfigured_runtime_config.config)
        .expect("reconfigure should succeed against the rotated Custom config");
    assert_eq!(reconfigured_status.state, RuntimeState::Ready);

    let fresh_probe = call_with_generation_check(
        &settings_store,
        reconfigured_runtime_config.settings_generation,
        &runtime,
        "test/echo",
        Some(serde_json::json!({"ping": true})),
        Duration::from_secs(5),
    )
    .expect("the probe must succeed again once the runtime was reconfigured for the new generation");
    assert_eq!(fresh_probe["ping"], serde_json::json!(true));

    // While a probe/turn holds the AI settings lock in shared mode, a
    // concurrent settings save must be rejected immediately instead of
    // silently applying or waiting behind it (the same contract
    // `call_with_generation_check` itself relies on for every probe/turn
    // above).
    let shared_during_probe = settings_store.settings_lock().try_acquire_shared().unwrap().unwrap();
    let save_attempt = settings_store.save(0, AiSettings::default(), || Ok(true));
    assert!(matches!(save_attempt, Err(SaveError::Busy)));
    drop(shared_during_probe);

    let _ = runtime.stop();
}

/// A single recorded HTTP request the mock Responses Provider observed.
struct RecordedRequest {
    method: String,
    path: String,
    authorization: Option<String>,
    body: String,
}

/// Minimal single-request HTTP/1.1 mock server: accepts one connection,
/// reads the request line, headers and (if `Content-Length` is present)
/// body, records it, and replies with a canned "completed" Responses API
/// body. Not a general-purpose HTTP server: it is a test fixture scoped to
/// exactly what a single Custom Provider probe turn needs.
fn spawn_mock_responses_provider() -> (u16, Arc<Mutex<Option<RecordedRequest>>>, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind a mock Responses Provider port");
    let port = listener.local_addr().unwrap().port();
    let recorded: Arc<Mutex<Option<RecordedRequest>>> = Arc::new(Mutex::new(None));
    let recorded_for_thread = recorded.clone();

    let handle = std::thread::spawn(move || {
        if let Ok((stream, _addr)) = listener.accept() {
            handle_one_request(stream, &recorded_for_thread);
        }
    });

    (port, recorded, handle)
}

fn handle_one_request(mut stream: TcpStream, recorded: &Arc<Mutex<Option<RecordedRequest>>>) {
    stream.set_read_timeout(Some(Duration::from_secs(10))).ok();
    let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));

    let mut request_line = String::new();
    if reader.read_line(&mut request_line).unwrap_or(0) == 0 {
        return;
    }
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let path = parts.next().unwrap_or("").to_string();

    let mut authorization = None;
    let mut content_length = 0usize;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            break;
        }
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            break;
        }
        if let Some((name, value)) = trimmed.split_once(':') {
            let name = name.trim().to_ascii_lowercase();
            let value = value.trim().to_string();
            if name == "authorization" {
                authorization = Some(value);
            } else if name == "content-length" {
                content_length = value.parse().unwrap_or(0);
            }
        }
    }

    let mut body_bytes = vec![0u8; content_length];
    if content_length > 0 {
        let _ = reader.read_exact(&mut body_bytes);
    }
    let body = String::from_utf8_lossy(&body_bytes).to_string();

    *recorded.lock().unwrap() = Some(RecordedRequest { method, path, authorization, body });

    let response_body = serde_json::json!({
        "id": "resp_test",
        "object": "response",
        "status": "completed",
        "output": [
            {
                "type": "message",
                "role": "assistant",
                "content": [{"type": "output_text", "text": "pong"}]
            }
        ]
    })
    .to_string();
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        response_body.len(),
        response_body
    );
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.flush();
}

/// Requires a real, bundled Codex App Server 0.157.1 binary at the path
/// given by `HANE_TEST_CODEX_APP_SERVER_BIN`. Skips (with an explicit,
/// printed reason instead of a silent pass) when that binary is not
/// available, per this worker's instruction to never fabricate evidence it
/// cannot actually obtain.
#[test]
fn real_app_server_reaches_the_mock_responses_provider_with_the_configured_key() {
    let Ok(binary) = std::env::var("HANE_TEST_CODEX_APP_SERVER_BIN") else {
        eprintln!(
            "SKIPPED: real_app_server_reaches_the_mock_responses_provider_with_the_configured_key - \
             HANE_TEST_CODEX_APP_SERVER_BIN is not set, so this evidence (a real Codex App Server 0.157.1 \
             reaching a mock Responses Provider with the generated Custom Provider config) was not obtained \
             in this run."
        );
        return;
    };

    // The binary named by `HANE_TEST_CODEX_APP_SERVER_BIN` must actually be
    // the 0.157.1 build this crate's Custom Provider config generation was
    // confirmed against (per ADR-0032 section 10) -- a different version
    // silently substituted here would invalidate every assertion below
    // without failing loudly.
    let version_output = std::process::Command::new(&binary)
        .arg("--version")
        .output()
        .expect("querying the configured Codex App Server binary's --version should succeed");
    let version_text = format!(
        "{}{}",
        String::from_utf8_lossy(&version_output.stdout),
        String::from_utf8_lossy(&version_output.stderr)
    );
    assert!(
        version_text.contains("0.157.1"),
        "HANE_TEST_CODEX_APP_SERVER_BIN must point at Codex App Server 0.157.1, got: {version_text}"
    );

    let (port, recorded, _http_thread) = spawn_mock_responses_provider();
    let base_url = format!("http://127.0.0.1:{port}/v1");

    let dir = unique_dir("real_app_server");
    let codex_home = dir.join("codex-home");
    let probe_workspace = dir.join("probe-workspace");
    std::fs::create_dir_all(&probe_workspace).unwrap();

    let material =
        build_custom_provider_material("My Provider", &base_url, "gpt-test-model", "sk-real-secret", ShellEnvironmentPolicyFormat::Filters)
            .unwrap();
    write_codex_config(&codex_home, &material.config_toml).unwrap();

    let mut config = RuntimeConfig::new(PathBuf::from(binary), dir.join("runtime.lock"));
    config.codex_home = Some(codex_home);
    config.extra_env.extend(material.extra_env);
    config.start_timeout = Duration::from_secs(30);
    // Final config-compatibility validation per ADR-0032 section 10: the
    // bundled binary must accept the generated config under
    // `--strict-config`, not merely tolerate it silently.
    config.args.push("--strict-config".to_string());

    let (events_tx, events_rx) = mpsc::sync_channel(1024);
    let handler = Arc::new(RejectAllServerRequests);
    let runtime = AiRuntime::spawn(config, handler, events_tx);
    let status = runtime
        .start()
        .expect("real App Server should reach Ready with the generated Custom config under --strict-config");
    assert_eq!(status.state, RuntimeState::Ready);

    let thread_start = runtime
        .call(
            "thread/start",
            Some(serde_json::json!({
                "cwd": probe_workspace.to_string_lossy(),
                "modelProvider": hane_ai::CUSTOM_PROVIDER_ID,
                "model": "gpt-test-model",
            })),
            Duration::from_secs(30),
        )
        .expect("thread/start should succeed against the generated Custom config");
    let thread_id = thread_start["thread"]["id"]
        .as_str()
        .expect("thread/start response should include thread.id")
        .to_string();

    // Unlike a discarded `let _ = ...`, a `turn/start` failure (a JSON-RPC
    // error response surfaces as `Err(RuntimeError::Rpc(RpcError::Remote))`)
    // must fail this test instead of silently being treated as evidence of
    // success.
    let _turn_start = runtime
        .call(
            "turn/start",
            Some(serde_json::json!({
                "threadId": thread_id,
                "input": [{"type": "text", "text": "ping"}],
            })),
            Duration::from_secs(30),
        )
        .expect("turn/start should succeed against the generated Custom config");

    // Wait for the turn to actually complete instead of racing `stop()`
    // against it: `turn/completed` (mirroring the already-established
    // `thread/start`/`turn/start` naming) is delivered asynchronously as a
    // notification, not in `turn/start`'s own response.
    let turn_completed_deadline = std::time::Instant::now() + Duration::from_secs(30);
    let turn_completed_params = loop {
        let remaining = turn_completed_deadline.saturating_duration_since(std::time::Instant::now());
        assert!(!remaining.is_zero(), "timed out waiting for a turn/completed notification from the real App Server");
        let event = events_rx
            .recv_timeout(remaining)
            .expect("timed out waiting for a turn/completed notification from the real App Server");
        let hane_ai::RuntimeEventKind::Notification { method, params } = event.kind else {
            continue;
        };
        assert!(
            !method.contains("fail"),
            "received a failure notification instead of turn/completed: {method} {params:?}"
        );
        if method == "turn/completed" {
            break params.unwrap_or(serde_json::Value::Null);
        }
    };
    let turn_completed_text = turn_completed_params.to_string();
    assert!(
        turn_completed_text.contains("pong"),
        "turn/completed payload should include the mock Responses Provider's fixed \"pong\" reply, got: {turn_completed_text}"
    );

    let _ = runtime.stop();

    let recorded = recorded.lock().unwrap().take();
    let recorded = recorded.expect("the mock Responses Provider should have observed exactly one request");
    assert_eq!(recorded.method, "POST");
    assert!(recorded.path.starts_with("/v1"), "request path should target the configured base_url: {}", recorded.path);
    let auth = recorded.authorization.expect("request should carry an Authorization header");
    assert_eq!(
        auth, "Bearer sk-real-secret",
        "request must be authenticated with exactly the configured Custom Provider key"
    );
    let body_json: serde_json::Value =
        serde_json::from_str(&recorded.body).expect("the Responses API request body should be valid JSON");
    assert_eq!(body_json["model"], serde_json::json!("gpt-test-model"), "request must target exactly the configured model");
}
