//! Custom Provider integration tests.
//!
//! `custom_provider_key_reaches_only_the_spawned_child_process_environment`,
//! `rotating_the_custom_provider_key_never_reaches_the_old_child`,
//! `internal_path_save_reconfigure_and_generation_gated_probe_compose_end_to_end`,
//! and `generation_gate_never_runs_the_closure_before_the_runtime_is_actually_ready_for_the_matching_generation`
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
    update_custom_credential, with_generation_checked_lock, write_codex_config, ActiveConnection, AiPaths, AiSettings,
    AiSettingsStore, AiRuntime, ChatGptConnectionSettings, ConnectError, CredentialJournal, CredentialStore,
    CustomConnectionSettings, FakeCredentialStore, RejectAllServerRequests, RuntimeConfig, RuntimeError, RuntimeState,
    SaveError, ShellEnvironmentPolicyFormat, CUSTOM_PROVIDER_ENV_KEY,
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

    let reconfigured = runtime.reconfigure(second_config, 1).expect("reconfigure should succeed");
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

/// ADR-0032 section 8's "runtime owner lock → AI settings lock" order, and
/// the "active runtime already owns the lock" half of it specifically:
/// while this `AiRuntime` is `Ready` (and therefore already holds the
/// runtime owner lock itself), a completely separate `OwnerLock` instance
/// for the same path must not be able to acquire it, and an ordinary
/// settings save must instead go through `AiRuntime::with_owner_lock` to
/// borrow proof of that already-held ownership -- never by attempting (and
/// failing) a second, independent acquisition of the same lock file.
#[test]
fn with_owner_lock_lets_a_normal_settings_save_happen_while_this_runtime_holds_the_owner_lock() {
    let (config, dir) = fake_server_config("with_owner_lock_active");
    let owner_lock_path = config.owner_lock_path.clone();
    let runtime = spawn_and_start(config);

    let external = hane_ai::OwnerLock::new(&owner_lock_path);
    assert!(
        external.try_acquire().unwrap().is_none(),
        "a separate OwnerLock instance must not be able to acquire the lock while this runtime is active"
    );

    let settings_store = AiSettingsStore::new(dir.join("ai-settings.json"), dir.join("ai-settings.lock"));
    let journal = CredentialJournal::new(dir.join("credential-journal.json"));
    let credential_store = FakeCredentialStore::new();

    let saved = runtime
        .with_owner_lock(move |owner| {
            let owner = owner.expect("the active runtime must hold the owner lock while Ready");
            update_custom_credential(&settings_store, owner, &journal, &credential_store, 0, None, "sk-active", |new_ref| {
                custom_settings_for(Some(new_ref.clone()))
            })
        })
        .expect("with_owner_lock should run its closure on the coordinator thread")
        .expect("the settings save should succeed while this runtime already owns the runtime owner lock");
    assert_eq!(saved.revision, 1);

    let _ = runtime.stop();

    // Once stopped, the owner lock is released and a separate acquire now
    // succeeds.
    assert!(external.try_acquire().unwrap().is_some());
}

/// The other half of ADR-0032 section 8's ordering contract: when no
/// runtime is active yet, a caller try-locks the runtime owner lock itself,
/// uses it to guard a settings save, and then hands that exact same guard
/// to `AiRuntime::start_with_owner_lock` -- never dropping it (and
/// therefore never releasing the lock to a would-be new owner) in between.
#[test]
fn start_with_owner_lock_reuses_an_externally_acquired_owner_lock_without_a_gap() {
    let (config, dir) = fake_server_config("start_with_owner_lock");
    let owner_lock_path = config.owner_lock_path.clone();

    let owner_lock = hane_ai::OwnerLock::new(&owner_lock_path);
    let owner = owner_lock.try_acquire().unwrap().expect("no other process holds the owner lock yet");

    let settings_store = AiSettingsStore::new(dir.join("ai-settings.json"), dir.join("ai-settings.lock"));
    let journal = CredentialJournal::new(dir.join("credential-journal.json"));
    let credential_store = FakeCredentialStore::new();
    let saved = update_custom_credential(&settings_store, &owner, &journal, &credential_store, 0, None, "sk-boot", |new_ref| {
        custom_settings_for(Some(new_ref.clone()))
    })
    .expect("the settings save should succeed while holding the freshly acquired owner lock");
    assert_eq!(saved.revision, 1);

    let (events_tx, _events_rx) = mpsc::sync_channel(64);
    let handler = Arc::new(RejectAllServerRequests);
    let runtime = AiRuntime::spawn(config, handler, events_tx);
    let status = runtime.start_with_owner_lock(owner).expect("start_with_owner_lock should succeed");
    assert_eq!(status.state, RuntimeState::Ready);

    // A separate `OwnerLock` instance still cannot acquire it: the same
    // guard handed to `start_with_owner_lock` is still held, continuously,
    // by the now-running coordinator.
    let external = hane_ai::OwnerLock::new(&owner_lock_path);
    assert!(external.try_acquire().unwrap().is_none());

    let _ = runtime.stop();
    assert!(external.try_acquire().unwrap().is_some());
}

/// Structural rejection of a non-owner: while a first `OwnerLock` instance
/// holds the lock, a second, independent instance for the same path cannot
/// acquire it and therefore has no way to obtain the `&OwnerLockGuard`
/// proof `update_custom_credential`/`AiSettingsStore::save` require. Once
/// the first is dropped, the lock becomes available and the new owner can
/// save normally.
#[test]
fn a_non_owner_cannot_obtain_the_proof_required_to_save_settings_until_the_owner_releases_the_lock() {
    let dir = unique_dir("non_owner_rejected");
    let owner_lock_path = dir.join("owner.lock");
    let first_owner_lock = hane_ai::OwnerLock::new(&owner_lock_path);
    let first = first_owner_lock.try_acquire().unwrap().unwrap();

    let second_owner_lock = hane_ai::OwnerLock::new(&owner_lock_path);
    assert!(
        second_owner_lock.try_acquire().unwrap().is_none(),
        "a non-owner must be rejected before it can ever obtain the proof required to call a settings-write API"
    );

    drop(first);
    let second = second_owner_lock.try_acquire().unwrap().expect("the lock becomes available once the owner releases it");
    let settings_store = AiSettingsStore::new(dir.join("ai-settings.json"), dir.join("ai-settings.lock"));
    let journal = CredentialJournal::new(dir.join("credential-journal.json"));
    let credential_store = FakeCredentialStore::new();
    update_custom_credential(&settings_store, &second, &journal, &credential_store, 0, None, "sk-new-owner", |new_ref| {
        custom_settings_for(Some(new_ref.clone()))
    })
    .expect("the new owner should be able to save once it holds the lock");
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
    let settings_store = Arc::new(AiSettingsStore::new(dir.join("ai-settings.json"), dir.join("ai-settings.lock")));
    let journal = CredentialJournal::new(dir.join("credential-journal.json"));
    let credential_store: Arc<dyn CredentialStore> = Arc::new(FakeCredentialStore::new());
    let binary = PathBuf::from(env!("CARGO_BIN_EXE_fake_app_server"));
    let owner_lock_path = dir.join("runtime.lock");

    // Step 0: acquire the runtime owner lock ourselves, per ADR-0032
    // section 8's "try-lock before an out-of-runtime save, hold it through
    // any necessary runtime (re)generation" contract -- there is no
    // `AiRuntime` yet to hold it internally for this very first save.
    let owner = hane_ai::OwnerLock::new(&owner_lock_path).try_acquire().unwrap().unwrap();

    // Step 1: settings/credential save.
    let saved =
        update_custom_credential(&settings_store, &owner, &journal, &*credential_store, 0, None, "sk-first", |new_ref| {
            custom_settings_for(Some(new_ref.clone()))
        })
        .expect("initial credential save should succeed");

    // Step 2: settings reload, then Custom config generation from it.
    let reloaded = settings_store.load().unwrap();
    assert_eq!(reloaded, saved);
    let configured = build_runtime_config_for_active_connection(
        &reloaded,
        &*credential_store,
        &paths,
        binary.clone(),
        owner_lock_path.clone(),
        ShellEnvironmentPolicyFormat::Filters,
    )
    .expect("building the runtime config for the freshly saved settings should succeed");

    // Step 3: runtime spawn/start against the generated config, handing the
    // same already-held owner lock guard over to the coordinator so it is
    // never released to a would-be new owner between the settings save
    // above and this runtime becoming its owner.
    let (events_tx, _events_rx) = mpsc::sync_channel(64);
    let handler = Arc::new(RejectAllServerRequests);
    let runtime =
        AiRuntime::spawn_with_configured_settings_generation(configured.config, configured.settings_generation, handler, events_tx);
    let status = runtime
        .start_with_owner_lock(owner)
        .expect("start_with_owner_lock should succeed against the generated Custom config");
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
    // reaching the runtime. The runtime is already `Ready` and therefore
    // already holds the runtime owner lock itself, so this save borrows
    // proof of that ownership from the running coordinator via
    // `with_owner_lock` instead of attempting (and failing) a second,
    // independent acquisition of the same lock file.
    let old_ref = saved.custom.as_ref().unwrap().credential_ref.clone().unwrap();
    let saved_revision = saved.revision;
    let rotation_settings_store = settings_store.clone();
    let rotation_credential_store = credential_store.clone();
    let rotated = runtime
        .with_owner_lock(move |owner| {
            let owner = owner.expect("the active runtime must hold the owner lock while Ready");
            update_custom_credential(
                &rotation_settings_store,
                owner,
                &journal,
                &*rotation_credential_store,
                saved_revision,
                Some(old_ref),
                "sk-second",
                |new_ref| custom_settings_for(Some(new_ref.clone())),
            )
        })
        .expect("with_owner_lock should run its closure on the coordinator thread")
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
        &*credential_store,
        &paths,
        binary,
        owner_lock_path,
        ShellEnvironmentPolicyFormat::Filters,
    )
    .expect("building the runtime config for the rotated settings should succeed");

    let reconfigured_status = runtime
        .reconfigure(reconfigured_runtime_config.config, reconfigured_runtime_config.settings_generation)
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
    // above). This save still needs the runtime owner lock proof, borrowed
    // the same way as the rotation above.
    let shared_during_probe = settings_store.settings_lock().try_acquire_shared().unwrap().unwrap();
    let busy_settings_store = settings_store.clone();
    let save_attempt = runtime.with_owner_lock(move |owner| {
        let owner = owner.expect("the active runtime must hold the owner lock while Ready");
        busy_settings_store.save(owner, 0, AiSettings::default(), || Ok(true))
    });
    assert!(matches!(save_attempt, Ok(Err(SaveError::Busy))));
    drop(shared_during_probe);

    let _ = runtime.stop();
}

/// Regression coverage for the root cause behind a generation-gated
/// operation (`with_generation_checked_lock`) being able to run its closure
/// merely because the persisted `settings_generation` and the caller's own
/// `configured_settings_generation` happen to agree with what
/// `AiRuntime::reconfigure` has *structurally* swapped `config` to -- even
/// though the runtime itself has not actually confirmed a matching `Ready`
/// state yet (still `Stopped`, mid-restart, or the restart failed). Uses a
/// real `fake_app_server` child (via `FAKE_SERVER_MODE=never_respond`) to
/// observe the genuinely transient `Starting`/`Initializing` window, not just
/// the terminal `Stopped`/`Failed` states. Confirms the closure passed to
/// `with_generation_checked_lock` is never invoked for `Stopped`, `Starting`/
/// `Initializing`, a failed start, or a generation mismatch while `Ready`,
/// and is invoked exactly once the runtime is actually `Ready` for the exact
/// matching generation.
#[test]
fn generation_gate_never_runs_the_closure_before_the_runtime_is_actually_ready_for_the_matching_generation() {
    let dir = unique_dir("generation_gate_ready");
    let settings_store = AiSettingsStore::new(dir.join("ai-settings.json"), dir.join("ai-settings.lock"));
    let owner = hane_ai::OwnerLock::new(dir.join("owner.lock")).try_acquire().unwrap().unwrap();
    let mut first = AiSettings::default();
    first.chatgpt.model_id = Some("gpt-a".to_string());
    let saved = settings_store.save(&owner, 0, first, || Ok(true)).unwrap();
    assert_eq!(saved.settings_generation, 1);
    drop(owner);

    let closure_calls = AtomicU64::new(0);

    let (mut hang_config, _hang_dir) = fake_server_config("generation_gate_stopped_starting_failed");
    hang_config.extra_env.push(("FAKE_SERVER_MODE".to_string(), "never_respond".to_string()));
    hang_config.start_timeout = Duration::from_millis(500);
    let (events_tx, _events_rx) = mpsc::sync_channel(64);
    let handler = Arc::new(RejectAllServerRequests);
    let runtime = AiRuntime::spawn_with_configured_settings_generation(hang_config, 1, handler, events_tx);

    let run_gate = |generation: u64| -> Result<(), ConnectError> {
        with_generation_checked_lock(&settings_store, &runtime, generation, || {
            closure_calls.fetch_add(1, Ordering::SeqCst);
        })
    };

    // --- Stopped: never started yet. ---
    assert_eq!(runtime.snapshot().state, RuntimeState::Stopped);
    let calls_before = closure_calls.load(Ordering::SeqCst);
    let result = run_gate(1);
    assert!(
        matches!(result, Err(ConnectError::Runtime(RuntimeError::NotReady))),
        "Stopped: expected NotReady, got {result:?}"
    );
    assert_eq!(closure_calls.load(Ordering::SeqCst), calls_before, "closure must not run while Stopped");

    // --- Starting/Initializing: start() in flight, handshake withheld. ---
    std::thread::scope(|scope| {
        let start_handle = scope.spawn(|| runtime.start());

        let mut observed_in_flight_state = None;
        let deadline = std::time::Instant::now() + Duration::from_millis(400);
        while std::time::Instant::now() < deadline {
            let state = runtime.snapshot().state;
            if matches!(state, RuntimeState::Starting | RuntimeState::Initializing) {
                observed_in_flight_state = Some(state);
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        let observed_in_flight_state =
            observed_in_flight_state.expect("should observe Starting/Initializing before start_timeout elapses");

        let calls_before = closure_calls.load(Ordering::SeqCst);
        let result = run_gate(1);
        assert!(
            matches!(result, Err(ConnectError::Runtime(RuntimeError::NotReady))),
            "{observed_in_flight_state:?}: expected NotReady, got {result:?}"
        );
        assert_eq!(
            closure_calls.load(Ordering::SeqCst),
            calls_before,
            "closure must not run while {observed_in_flight_state:?}"
        );

        let start_result = start_handle.join().unwrap();
        assert!(
            matches!(start_result, Err(RuntimeError::Handshake(_))),
            "expected the withheld handshake to time out, got {start_result:?}"
        );
    });

    // --- Failed: the start attempt above timed out. ---
    assert_eq!(runtime.snapshot().state, RuntimeState::Failed);
    let calls_before = closure_calls.load(Ordering::SeqCst);
    let result = run_gate(1);
    assert!(
        matches!(result, Err(ConnectError::Runtime(RuntimeError::NotReady))),
        "Failed: expected NotReady, got {result:?}"
    );
    assert_eq!(closure_calls.load(Ordering::SeqCst), calls_before, "closure must not run after a failed start");

    // --- Ready for the matching generation: reconfigure onto a working config. ---
    let (working_config, _working_dir) = fake_server_config("generation_gate_ready_after_reconfigure");
    let ready_status = runtime
        .reconfigure(working_config, 1)
        .expect("reconfigure onto a working config should succeed once the handshake is not withheld");
    assert_eq!(ready_status.state, RuntimeState::Ready);

    let calls_before = closure_calls.load(Ordering::SeqCst);
    let result = run_gate(1);
    assert!(result.is_ok(), "Ready with the matching generation should allow the closure to run, got {result:?}");
    assert_eq!(closure_calls.load(Ordering::SeqCst), calls_before + 1, "closure must run exactly once now");

    // --- Ready but a generation mismatch: still rejected, closure still not run. ---
    let calls_before = closure_calls.load(Ordering::SeqCst);
    let result = run_gate(2);
    assert!(
        matches!(result, Err(ConnectError::GenerationMismatch { .. })),
        "a generation mismatch while Ready should still be rejected, got {result:?}"
    );
    assert_eq!(closure_calls.load(Ordering::SeqCst), calls_before, "closure must not run on a generation mismatch");

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
/// body, records it, and replies with exactly `response` (a full raw
/// HTTP/1.1 response, status line through body). Not a general-purpose HTTP
/// server: it is a test fixture scoped to exactly what a single Custom
/// Provider probe turn needs, letting callers inject any upstream reply
/// (success, 401/403/429, malformed, or Responses-API-incompatible) to
/// confirm the App Server surfaces each distinctly rather than always
/// reporting success.
fn spawn_mock_responses_provider(response: String) -> (u16, Arc<Mutex<Option<RecordedRequest>>>, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind a mock Responses Provider port");
    let port = listener.local_addr().unwrap().port();
    let recorded: Arc<Mutex<Option<RecordedRequest>>> = Arc::new(Mutex::new(None));
    let recorded_for_thread = recorded.clone();

    let handle = std::thread::spawn(move || {
        if let Ok((stream, _addr)) = listener.accept() {
            handle_one_request(stream, &recorded_for_thread, &response);
        }
    });

    (port, recorded, handle)
}

/// A full raw HTTP/1.1 response with a JSON body and the given status line
/// (e.g. `"200 OK"`, `"401 Unauthorized"`).
fn json_response(status_line: &str, body: &serde_json::Value) -> String {
    let body_text = body.to_string();
    format!(
        "HTTP/1.1 {status_line}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body_text.len(),
        body_text
    )
}

/// A full raw HTTP/1.1 response whose body is not necessarily valid JSON,
/// used to inject a malformed upstream reply.
fn raw_response(status_line: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status_line}\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    )
}

/// A full raw HTTP/1.1 `text/event-stream` response whose body is the
/// concatenation of one Server-Sent Event per `data:` line for each of
/// `events`, in order. The Responses API (`wire_api = "responses"`) streams
/// its reply this way rather than returning one plain JSON body: a bundled
/// Codex App Server's Responses client expects this framing, and a plain
/// `200 application/json` body with a fully "completed" response (as a
/// non-streaming Responses/Chat-Completions-style reply would use) is never
/// recognized as any event, so no `turn/completed` (nor any failure
/// notification) is ever observed for it.
fn sse_response(status_line: &str, events: &[serde_json::Value]) -> String {
    let mut body = String::new();
    for event in events {
        body.push_str("data: ");
        body.push_str(&event.to_string());
        body.push_str("\n\n");
    }
    format!(
        "HTTP/1.1 {status_line}\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    )
}

/// The fixed "completed" Responses API stream the successful test path
/// expects to see echoed back, as `text`, in `turn/completed`: the standard
/// OpenAI Responses API streaming event sequence for one simple assistant
/// text message ("pong"), ending in the terminal `response.completed` event
/// carrying the full response object.
fn fixed_pong_response() -> String {
    let item = serde_json::json!({
        "id": "msg_test",
        "type": "message",
        "status": "completed",
        "role": "assistant",
        "content": [{"type": "output_text", "text": "pong"}]
    });
    let completed_response = serde_json::json!({
        "id": "resp_test",
        "object": "response",
        "status": "completed",
        "output": [item]
    });
    sse_response(
        "200 OK",
        &[
            serde_json::json!({
                "type": "response.created",
                "response": {"id": "resp_test", "object": "response", "status": "in_progress", "output": []}
            }),
            serde_json::json!({
                "type": "response.output_item.added",
                "output_index": 0,
                "item": {"id": "msg_test", "type": "message", "status": "in_progress", "role": "assistant", "content": []}
            }),
            serde_json::json!({
                "type": "response.content_part.added",
                "item_id": "msg_test",
                "output_index": 0,
                "content_index": 0,
                "part": {"type": "output_text", "text": ""}
            }),
            serde_json::json!({
                "type": "response.output_text.delta",
                "item_id": "msg_test",
                "output_index": 0,
                "content_index": 0,
                "delta": "pong"
            }),
            serde_json::json!({
                "type": "response.output_text.done",
                "item_id": "msg_test",
                "output_index": 0,
                "content_index": 0,
                "text": "pong"
            }),
            serde_json::json!({
                "type": "response.content_part.done",
                "item_id": "msg_test",
                "output_index": 0,
                "content_index": 0,
                "part": {"type": "output_text", "text": "pong"}
            }),
            serde_json::json!({
                "type": "response.output_item.done",
                "output_index": 0,
                "item": item
            }),
            serde_json::json!({
                "type": "response.completed",
                "response": completed_response
            }),
        ],
    )
}

fn handle_one_request(mut stream: TcpStream, recorded: &Arc<Mutex<Option<RecordedRequest>>>, response: &str) {
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

    let _ = stream.write_all(response.as_bytes());
    let _ = stream.flush();
}

/// Prints an explicit, distinctly grep-able skip notice for a test that
/// requires a real, bundled Codex App Server 0.157.1 binary
/// (`HANE_TEST_CODEX_APP_SERVER_BIN`), instead of silently passing. The
/// leading `REQUIRED_REAL_APP_SERVER_EVIDENCE_NOT_OBTAINED` marker is fixed
/// and must not be treated as, or confused with, an actual pass: this test
/// returning green only means it did not assert anything, not that the
/// required real-App-Server evidence was obtained.
fn print_real_app_server_evidence_skipped(test_name: &str, what_was_not_obtained: &str) {
    eprintln!(
        "REQUIRED_REAL_APP_SERVER_EVIDENCE_NOT_OBTAINED: {test_name} - HANE_TEST_CODEX_APP_SERVER_BIN is not set, \
         so {what_was_not_obtained} was not obtained in this run. A green result for this test must not be read as \
         satisfying ADR-0032's real-response acceptance evidence; that evidence must be obtained separately with \
         HANE_TEST_CODEX_APP_SERVER_BIN set to a real Codex App Server 0.157.1 binary."
    );
}

/// Requires a real, bundled Codex App Server 0.157.1 binary at the path
/// given by `HANE_TEST_CODEX_APP_SERVER_BIN`. Skips (with an explicit,
/// printed reason instead of a silent pass) when that binary is not
/// available, per this worker's instruction to never fabricate evidence it
/// cannot actually obtain.
#[test]
fn real_app_server_reaches_the_mock_responses_provider_with_the_configured_key() {
    let Ok(binary) = std::env::var("HANE_TEST_CODEX_APP_SERVER_BIN") else {
        print_real_app_server_evidence_skipped(
            "real_app_server_reaches_the_mock_responses_provider_with_the_configured_key",
            "evidence that a real Codex App Server 0.157.1 reaches a mock Responses Provider with the generated \
             Custom Provider config and returns the fixed \"pong\" reply via turn/completed",
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

    let (port, recorded, _http_thread) = spawn_mock_responses_provider(fixed_pong_response());
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
    // `HANE_TEST_CODEX_APP_SERVER_BIN` names the `codex` CLI, not the
    // standalone App Server `RuntimeConfig::new`'s default args assume: it
    // requires the `app-server` subcommand before its own flags. Final
    // config-compatibility validation per ADR-0032 section 10: the bundled
    // binary must accept the generated config under `--strict-config`, not
    // merely tolerate it silently.
    config.args = vec!["app-server".to_string(), "--strict-config".to_string(), "--listen".to_string(), "stdio://".to_string()];

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
    assert_mock_provider_received_the_probe_request(recorded, "fixed pong success case");
}

/// Asserts that the mock Responses Provider actually received exactly one
/// request shaped like a genuine Custom Provider probe -- `POST` to exactly
/// `/v1/responses` under the configured `base_url`, authenticated with
/// exactly the configured key, and targeting exactly the configured model --
/// before a caller goes on to check how the turn's outcome was surfaced.
/// Without this,
/// a `turn/start` RPC error or a timeout that occurs *before* the App Server
/// ever reaches the Responses Provider would let a failure test pass without
/// having exercised the Provider-error path it claims to.
fn assert_mock_provider_received_the_probe_request(recorded: Option<RecordedRequest>, context: &str) {
    let recorded = recorded
        .unwrap_or_else(|| panic!("{context}: the mock Responses Provider should have observed exactly one request"));
    assert_eq!(recorded.method, "POST", "{context}: request method");
    assert_eq!(
        recorded.path, "/v1/responses",
        "{context}: request path should target exactly the Responses API endpoint under the configured base_url"
    );
    let auth = recorded
        .authorization
        .unwrap_or_else(|| panic!("{context}: request should carry an Authorization header"));
    assert_eq!(auth, "Bearer sk-real-secret", "{context}: request must be authenticated with exactly the configured key");
    let body_json: serde_json::Value = serde_json::from_str(&recorded.body)
        .unwrap_or_else(|e| panic!("{context}: the Responses API request body should be valid JSON: {e}"));
    assert_eq!(body_json["model"], serde_json::json!("gpt-test-model"), "{context}: request must target exactly the configured model");
}

/// Spawns the real App Server (`binary`) against a Custom Provider config
/// pointed at a mock Responses Provider that always replies with
/// `mock_response`, runs one `thread/start`/`turn/start`, and reports
/// whether a `turn/completed` notification carrying the fixed "pong" success
/// text was observed within a bounded timeout, together with the request (if
/// any) the mock Responses Provider actually received. Any other outcome --
/// a `turn/start` RPC error, a `turn/completed`/other notification that does
/// *not* contain "pong", or timing out without ever observing
/// `turn/completed` -- is reported as `false` (not success): this crate
/// cannot assume in advance exactly which of those shapes the bundled App
/// Server uses to surface a given upstream failure, but "silently reports
/// success anyway" must never be one of them. Callers must separately check
/// the returned recorded request: a `false` result must mean the Provider
/// error was actually surfaced as a failure, not merely that the App Server
/// never reached the Provider at all.
fn observed_successful_turn_against_mock_response(
    binary: &str,
    dir_name: &str,
    mock_response: String,
) -> (bool, Option<RecordedRequest>) {
    let (port, recorded, _http_thread) = spawn_mock_responses_provider(mock_response);
    let base_url = format!("http://127.0.0.1:{port}/v1");

    let dir = unique_dir(dir_name);
    let codex_home = dir.join("codex-home");
    let probe_workspace = dir.join("probe-workspace");
    std::fs::create_dir_all(&probe_workspace).unwrap();

    let material = build_custom_provider_material(
        "My Provider",
        &base_url,
        "gpt-test-model",
        "sk-real-secret",
        ShellEnvironmentPolicyFormat::Filters,
    )
    .unwrap();
    write_codex_config(&codex_home, &material.config_toml).unwrap();

    let mut config = RuntimeConfig::new(PathBuf::from(binary), dir.join("runtime.lock"));
    config.codex_home = Some(codex_home);
    config.extra_env.extend(material.extra_env);
    config.start_timeout = Duration::from_secs(30);
    // See the matching comment in
    // `real_app_server_reaches_the_mock_responses_provider_with_the_configured_key`:
    // `HANE_TEST_CODEX_APP_SERVER_BIN` names the `codex` CLI, which needs the
    // `app-server` subcommand before `--strict-config`/`--listen stdio://`.
    config.args = vec!["app-server".to_string(), "--strict-config".to_string(), "--listen".to_string(), "stdio://".to_string()];

    let (events_tx, events_rx) = mpsc::sync_channel(1024);
    let handler = Arc::new(RejectAllServerRequests);
    let runtime = AiRuntime::spawn(config, handler, events_tx);
    let status = runtime
        .start()
        .expect("real App Server should reach Ready with the generated Custom config under --strict-config");
    assert_eq!(status.state, RuntimeState::Ready);

    let thread_start = runtime.call(
        "thread/start",
        Some(serde_json::json!({
            "cwd": probe_workspace.to_string_lossy(),
            "modelProvider": hane_ai::CUSTOM_PROVIDER_ID,
            "model": "gpt-test-model",
        })),
        Duration::from_secs(30),
    );
    let thread_start = match thread_start {
        Ok(v) => v,
        Err(_) => {
            let _ = runtime.stop();
            return (false, recorded.lock().unwrap().take());
        }
    };
    let Some(thread_id) = thread_start["thread"]["id"].as_str().map(str::to_string) else {
        let _ = runtime.stop();
        return (false, recorded.lock().unwrap().take());
    };

    let turn_start = runtime.call(
        "turn/start",
        Some(serde_json::json!({
            "threadId": thread_id,
            "input": [{"type": "text", "text": "ping"}],
        })),
        Duration::from_secs(30),
    );
    if turn_start.is_err() {
        let _ = runtime.stop();
        return (false, recorded.lock().unwrap().take());
    }

    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    let observed_success = loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            break false;
        }
        let event = match events_rx.recv_timeout(remaining) {
            Ok(event) => event,
            Err(_) => break false,
        };
        let hane_ai::RuntimeEventKind::Notification { method, params } = event.kind else {
            continue;
        };
        if method == "turn/completed" {
            let text = params.unwrap_or(serde_json::Value::Null).to_string();
            break text.contains("pong");
        }
        if method.contains("fail") {
            break false;
        }
    };

    let _ = runtime.stop();
    (observed_success, recorded.lock().unwrap().take())
}

#[test]
fn real_app_server_reports_failure_for_401_unauthorized_from_the_provider() {
    let Ok(binary) = std::env::var("HANE_TEST_CODEX_APP_SERVER_BIN") else {
        print_real_app_server_evidence_skipped(
            "real_app_server_reports_failure_for_401_unauthorized_from_the_provider",
            "evidence that a 401 Unauthorized response from the Responses Provider is surfaced as a failure",
        );
        return;
    };
    let response = json_response(
        "401 Unauthorized",
        &serde_json::json!({"error": {"message": "invalid api key", "type": "invalid_request_error"}}),
    );
    let (observed_success, recorded) =
        observed_successful_turn_against_mock_response(&binary, "real_app_server_401", response);
    assert_mock_provider_received_the_probe_request(recorded, "401 Unauthorized case");
    assert!(
        !observed_success,
        "a 401 Unauthorized response from the Responses Provider must never be reported as a successful turn/completed"
    );
}

#[test]
fn real_app_server_reports_failure_for_403_forbidden_from_the_provider() {
    let Ok(binary) = std::env::var("HANE_TEST_CODEX_APP_SERVER_BIN") else {
        print_real_app_server_evidence_skipped(
            "real_app_server_reports_failure_for_403_forbidden_from_the_provider",
            "evidence that a 403 Forbidden response from the Responses Provider is surfaced as a failure",
        );
        return;
    };
    let response = json_response(
        "403 Forbidden",
        &serde_json::json!({"error": {"message": "access denied", "type": "permission_error"}}),
    );
    let (observed_success, recorded) =
        observed_successful_turn_against_mock_response(&binary, "real_app_server_403", response);
    assert_mock_provider_received_the_probe_request(recorded, "403 Forbidden case");
    assert!(
        !observed_success,
        "a 403 Forbidden response from the Responses Provider must never be reported as a successful turn/completed"
    );
}

#[test]
fn real_app_server_reports_failure_for_429_rate_limited_from_the_provider() {
    let Ok(binary) = std::env::var("HANE_TEST_CODEX_APP_SERVER_BIN") else {
        print_real_app_server_evidence_skipped(
            "real_app_server_reports_failure_for_429_rate_limited_from_the_provider",
            "evidence that a 429 Too Many Requests response from the Responses Provider is surfaced as a failure",
        );
        return;
    };
    let response = json_response(
        "429 Too Many Requests",
        &serde_json::json!({"error": {"message": "rate limit exceeded", "type": "rate_limit_error"}}),
    );
    let (observed_success, recorded) =
        observed_successful_turn_against_mock_response(&binary, "real_app_server_429", response);
    assert_mock_provider_received_the_probe_request(recorded, "429 Too Many Requests case");
    assert!(
        !observed_success,
        "a 429 Too Many Requests response from the Responses Provider must never be reported as a successful turn/completed"
    );
}

#[test]
fn real_app_server_reports_failure_for_a_malformed_response_body_from_the_provider() {
    let Ok(binary) = std::env::var("HANE_TEST_CODEX_APP_SERVER_BIN") else {
        print_real_app_server_evidence_skipped(
            "real_app_server_reports_failure_for_a_malformed_response_body_from_the_provider",
            "evidence that a malformed (non-JSON) 200 response body from the Responses Provider is surfaced as a failure",
        );
        return;
    };
    let response = raw_response("200 OK", "this is not valid json {{{");
    let (observed_success, recorded) =
        observed_successful_turn_against_mock_response(&binary, "real_app_server_malformed", response);
    assert_mock_provider_received_the_probe_request(recorded, "malformed response body case");
    assert!(
        !observed_success,
        "a malformed, non-JSON 200 response body from the Responses Provider must never be reported as a \
         successful turn/completed"
    );
}

#[test]
fn real_app_server_reports_failure_for_a_responses_api_incompatible_body_from_the_provider() {
    let Ok(binary) = std::env::var("HANE_TEST_CODEX_APP_SERVER_BIN") else {
        print_real_app_server_evidence_skipped(
            "real_app_server_reports_failure_for_a_responses_api_incompatible_body_from_the_provider",
            "evidence that a well-formed JSON 200 response shaped like the (incompatible) Chat Completions API, \
             instead of the Responses API, from the Responses Provider is surfaced as a failure",
        );
        return;
    };
    // Well-formed JSON, but shaped like a Chat Completions response
    // (`choices[].message`) rather than the Responses API's `output[]` this
    // Custom Provider config declares `wire_api = "responses"` for.
    let response = json_response(
        "200 OK",
        &serde_json::json!({
            "id": "chatcmpl_test",
            "object": "chat.completion",
            "choices": [
                {"index": 0, "message": {"role": "assistant", "content": "pong"}, "finish_reason": "stop"}
            ]
        }),
    );
    let (observed_success, recorded) =
        observed_successful_turn_against_mock_response(&binary, "real_app_server_incompatible", response);
    assert_mock_provider_received_the_probe_request(recorded, "Responses-API-incompatible body case");
    assert!(
        !observed_success,
        "a Responses-API-incompatible (Chat Completions shaped) response body must never be reported as a \
         successful turn/completed"
    );
}
