#![cfg_attr(
    all(target_os = "windows", not(feature = "instrument")),
    windows_subsystem = "windows"
)]

#[cfg(any(
    target_os = "windows",
    target_os = "macos",
    not(feature = "instrument")
))]
use gpui::Focusable;
use gpui::{App, AppContext, Bounds, Global, WindowBounds, WindowOptions, px, size};
use hane_ai::{
    AiService, AiServiceConfig, EXPLICIT_OVERRIDE_ENV_VAR, OsCredentialStore,
    ShellEnvironmentPolicyFormat, SystemBrowserOpener, resolve_runtime_binary,
};
use hane_session::{FileStateStore, StateStores};
use hane_ui::{AiLifecycle, AiResume, AppAssets, AppUpdateController, EditorView, register_key_bindings};
use hane_update::clock::SystemClock;
use hane_update::identity::{InstallFacts, InstallId};
use hane_update::model::Target;
use hane_update::trust::TrustPolicy;
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[cfg(any(target_os = "macos", all(test, unix)))]
mod finder_open;
#[cfg(target_os = "macos")]
mod finder_service;

mod app_update;
mod https_transport;
mod limited_start;
mod network_state;
#[cfg(target_os = "windows")]
mod open_request;
#[cfg(test)]
mod proxy_curl_tests;
#[cfg(test)]
mod proxy_update_tests;
#[cfg(test)]
mod test_tls;
mod update_diagnostics;
mod update_gate;
mod update_restart;

#[cfg(feature = "instrument")]
mod instrument;

const DEFAULT_DOCUMENT: &str = "# Hane Phase 4\n\n日本語IME、範囲選択、Undo / Redo、Markdown記号の段階表示に加えて、画像、表、保存、自動保存、Recent Files、themeを試せます。\n\n![Hane feather](assets/phase4-feather.svg)\n\n| Feature | Status |\n|:---|---:|\n| Typora-style editing | ✓ |\n| Atomic autosave | ✓ |\n| Light / Dark theme | ✓ |\n\n## Polish\n\n画像と表も元Markdownを唯一の正として保持します。行へカーソルを移動するとsourceを編集できます。\n";

/// Owns the AI service for the application lifetime. It is the one place that
/// can stop the service explicitly (and wait for it) and start it again, which
/// an update restart needs: the service must be gone before the helper may
/// replace files, and must come back if the restart is given up.
struct AiOwner {
    service: ServiceSlot<AiService>,
}

impl AiOwner {
    fn new(service: Option<AiService>) -> Self {
        Self {
            service: ServiceSlot::new(service),
        }
    }

    fn handle(&self) -> Option<hane_ai::AiServiceHandle> {
        self.service.slot().as_ref().map(AiService::handle)
    }
}

/// The owned service plus whether a stop is still in flight. A stop that timed
/// out keeps counting as in flight until the service is really gone, so a new
/// service is never started next to a worker that may still hold the owner lock.
struct ServiceSlot<S> {
    service: Arc<std::sync::Mutex<Option<S>>>,
    stopping: Arc<std::sync::atomic::AtomicBool>,
    /// A resume asked for while a stop was still in flight. The stop thread
    /// runs it once, right after the old service is really gone.
    pending_start: Arc<std::sync::Mutex<Option<PendingStart<S>>>>,
}

type PendingStart<S> = Box<dyn FnOnce() -> Option<S> + Send>;

fn lock_ignoring_poison<T>(mutex: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl<S: Send + 'static> ServiceSlot<S> {
    fn new(service: Option<S>) -> Self {
        Self {
            service: Arc::new(std::sync::Mutex::new(service)),
            stopping: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            pending_start: Arc::new(std::sync::Mutex::new(None)),
        }
    }

    fn slot(&self) -> std::sync::MutexGuard<'_, Option<S>> {
        lock_ignoring_poison(&self.service)
    }

    fn stop(&self, timeout: std::time::Duration) -> Result<(), String> {
        use std::sync::atomic::Ordering;
        let mut slot = self.slot();
        if self.stopping.load(Ordering::SeqCst) {
            return Err("the AI service is still stopping".to_owned());
        }
        let Some(service) = slot.take() else {
            return Ok(());
        };
        self.stopping.store(true, Ordering::SeqCst);
        drop(slot);
        // Dropping the service joins its worker, which may wait for a running
        // runtime; do it on its own thread so the wait is bounded.
        let (done, finished) = std::sync::mpsc::channel();
        let stopping = Arc::clone(&self.stopping);
        let shared_slot = Arc::clone(&self.service);
        let pending_start = Arc::clone(&self.pending_start);
        let spawned = std::thread::Builder::new()
            .name("hane-ai-stop".to_owned())
            .spawn(move || {
                drop(service);
                // Under the slot lock, so a concurrent `resume` either sees the
                // stop as in flight (and leaves its start pending) or sees it
                // finished; the pending start then runs exactly once.
                let mut slot = lock_ignoring_poison(&shared_slot);
                stopping.store(false, Ordering::SeqCst);
                let pending = lock_ignoring_poison(&pending_start).take();
                if let Some(start) = pending
                    && slot.is_none()
                {
                    *slot = start();
                }
                drop(slot);
                let _ = done.send(());
            });
        if let Err(err) = spawned {
            // The closure (and the service) was dropped by the failed spawn.
            self.stopping.store(false, Ordering::SeqCst);
            return Err(err.to_string());
        }
        finished
            .recv_timeout(timeout)
            .map_err(|_| "the AI service did not stop in time".to_owned())
    }

    /// Starts a service only when none exists and no stop is in flight. An
    /// unfinished stop never gets a second service next to it: the start is
    /// kept and run once by the stop thread when the old service is gone;
    /// [`Self::poll_resume`] then reports how that start went. A start that
    /// fails is `Failed`, and a later explicit `resume` may try again.
    fn resume<H>(
        &self,
        start: impl FnOnce() -> Option<S> + Send + 'static,
        handle: impl Fn(&S) -> H,
    ) -> AiResume<H> {
        use std::sync::atomic::Ordering;
        let mut slot = self.slot();
        if self.stopping.load(Ordering::SeqCst) {
            let mut pending = lock_ignoring_poison(&self.pending_start);
            if pending.is_none() {
                *pending = Some(Box::new(start));
            }
            return AiResume::Stopping;
        }
        if slot.is_none() {
            *slot = start();
        }
        match slot.as_ref() {
            Some(service) => AiResume::Started(handle(service)),
            None => AiResume::Failed,
        }
    }

    /// Reports the outcome of a resume that was left pending. It only looks at
    /// the state and never starts a service.
    fn poll_resume<H>(&self, handle: impl Fn(&S) -> H) -> AiResume<H> {
        use std::sync::atomic::Ordering;
        let slot = self.slot();
        if self.stopping.load(Ordering::SeqCst) {
            return AiResume::Stopping;
        }
        match slot.as_ref() {
            Some(service) => AiResume::Started(handle(service)),
            None => AiResume::Failed,
        }
    }
}

impl AiLifecycle for AiOwner {
    fn stop(&self, timeout: std::time::Duration) -> Result<(), String> {
        self.service.stop(timeout)
    }

    fn resume(&self) -> AiResume {
        self.service.resume(start_ai_service, AiService::handle)
    }

    fn poll_resume(&self) -> AiResume {
        self.service.poll_resume(AiService::handle)
    }
}

struct AppOwnedAiService(#[allow(dead_code, reason = "owns the service for the app lifetime")] Arc<AiOwner>);

impl Global for AppOwnedAiService {}

struct AppOwnedUpdateService(#[allow(dead_code, reason = "owns the service for the app lifetime")] Arc<app_update::AppUpdateService>);

impl Global for AppOwnedUpdateService {}

/// Builds the app-owned update service. Construction reads no network, ZIP or
/// update state; the service communicates only after an explicit button press.
/// `None` (no card) when the install location or state root cannot be named.
fn start_update_service() -> Option<Arc<app_update::AppUpdateService>> {
    let state = FileStateStore::from_environment().ok()?;
    let exe = std::env::current_exe().ok()?;
    let exe = std::fs::canonicalize(&exe).unwrap_or(exe);
    let install_dir = exe.parent()?.to_path_buf();
    // A supported placement supplies the real install identity, the same one
    // the startup gate leases on, and keeps its receipts in the coordination
    // directory beside the install. Anything else keeps the interim identity;
    // such a placement never becomes applicable.
    // The failure is kept: a placement that cannot be resolved must not be
    // read as a normal unmanaged install (see `block_placement` below).
    let located = hane_update::platform::locate(&exe);
    let (install_id, receipts_dir) = match &located {
        Ok(placement) => {
            let receipts = hane_update::platform::lease::Coordination::receipts_dir_for(
                &placement.coordination_parent,
                &placement.install_id,
            );
            (placement.install_id.clone(), receipts)
        }
        Err(_) => {
            let volume = install_dir
                .components()
                .next()?
                .as_os_str()
                .to_string_lossy()
                .into_owned();
            let owner = std::env::var("USERNAME")
                .or_else(|_| std::env::var("USER"))
                .ok()
                .filter(|owner| !owner.is_empty())
                .unwrap_or_else(|| "unknown".to_owned());
            let install_id = InstallId::derive(&InstallFacts {
                os: std::env::consts::OS,
                volume_id: &volume,
                install_path: hane_update::identity::install_path_text(&install_dir).ok()?,
                owner: &owner,
            })
            .ok()?;
            let receipts = install_dir
                .join(install_id.coordination_dir_name())
                .join("receipts");
            (install_id, receipts)
        }
    };
    let updates_root = state.root().join("updates").join(install_id.as_str());
    // The standard build always uses the fixed GitHub HTTPS transport. Only the
    // opt-in `validation-feed` build swaps in the loopback fixture transport;
    // signature, archive and apply checks below are unchanged.
    #[cfg(not(feature = "validation-feed"))]
    let transport: Arc<dyn hane_update::download::Transport> =
        Arc::new(https_transport::LazyHttpsTransport::new(Arc::new(network_context)));
    #[cfg(feature = "validation-feed")]
    let transport: Arc<dyn hane_update::download::Transport> = {
        let _ = &network_context;
        Arc::new(https_transport::validation_feed::ValidationFeedTransport::from_environment())
    };
    let service = Arc::new(app_update::AppUpdateService::new(
        app_update::UpdateServiceConfig {
            current_version: env!("CARGO_PKG_VERSION").to_owned(),
            target: Target::current().ok(),
            // Carries no key material until a maintainer embeds one, so a
            // production build reports `trust_unconfigured` and never offers
            // an update.
            policy: TrustPolicy::production(),
            install_id,
            updates_root,
            receipts_dir,
        },
        transport,
        Arc::new(SystemClock::new()),
    ));
    // Only a supported placement can hand a Ready update to the helper, and
    // even then "restart and update" is offered only once the trust policy is
    // configured; the production policy is not, so it stays fail-closed.
    match located {
        Ok(placement) => service.enable_restart(app_update::RestartWiring {
            placement,
            exe,
            state_root: state.root().to_path_buf(),
        }),
        // The interim receipts location is not a usable update location, so
        // the card reports the reason instead of fetching an archive first.
        Err(error) => service.block_placement(&error, &exe),
    }
    Some(service)
}

fn start_ai_service() -> Option<AiService> {
    let state = FileStateStore::from_environment().ok()?;
    let explicit_override = std::env::var_os(EXPLICIT_OVERRIDE_ENV_VAR);
    // A runtime is never resolved from the Hane ZIP/.app. The only startup
    // candidate is an explicit diagnostic override; otherwise the service
    // starts with an empty placeholder and resolves Hane's app-data install
    // directory without contacting the network. Resolution failure must
    // never prevent the Markdown editor from opening.
    let (binary_path, binary_resolution_error) = match explicit_override.as_deref() {
        Some(explicit_override) => {
            match resolve_runtime_binary(Some(explicit_override), Path::new("")) {
                Ok(resolved) => (resolved.binary_path, None),
                Err(code) => (PathBuf::new(), Some(code)),
            }
        }
        None => (PathBuf::new(), None),
    };
    AiService::spawn_with_network(
        AiServiceConfig {
            app_data_root: state.root().to_path_buf(),
            binary_path,
            binary_resolution_error,
            credential_store: Arc::new(OsCredentialStore::new("com.hide212131.hane.ai")),
            browser_opener: Arc::new(SystemBrowserOpener),
            shell_env_format: ShellEnvironmentPolicyFormat::Filters,
        },
        network_context(),
    )
    .ok()
}

/// The network input shared by the update check and the runtime download. The
/// proxy variables are read from the process environment exactly once, on the
/// first call (which happens at startup, before either service is created).
/// The settings are the ones saved on the network settings screen (a file
/// without them means the Environment mode). A stored configuration that
/// cannot be read makes every route fail explicitly; it never becomes a Direct
/// connection. The Manual Basic credentials are read from the OS secret store
/// only when a route needs them, and a read failure is an error rather than an
/// unauthenticated connection. A setting saved on the settings screen replaces
/// the snapshot (see [`AppNetworkApplier`]): an operation that starts later,
/// including an AI service restart, gets the new one, and an operation that
/// already took a snapshot keeps it.
fn network_context() -> hane_network::NetworkContext {
    network_state().current()
}

fn network_state() -> &'static network_state::NetworkState {
    static STATE: std::sync::OnceLock<network_state::NetworkState> = std::sync::OnceLock::new();
    STATE.get_or_init(|| {
        // A value that is not valid Unicode is kept lossily so a broken
        // proxy variable fails parsing instead of silently vanishing.
        let variables = std::env::vars_os().filter_map(|(name, value)| {
            Some((name.into_string().ok()?, value.to_string_lossy().into_owned()))
        });
        let environment = hane_network::ProxyEnvironment::from_vars(
            hane_network::EnvPlatform::current(),
            variables,
        );
        let saved = FileStateStore::from_environment()
            .map(|store| hane_session::SettingsRepository::load(&store).network)
            .unwrap_or_default();
        network_state::NetworkState::new(
            saved.effective_settings(),
            environment,
            Arc::new(hane_ui::OsProxyCredentialStore),
        )
    })
}

/// Hands a network setting saved on the settings screen to the services. It
/// records the setting only: it never starts a login, runtime download, update
/// check or text transform. The AI service takes a new network revision, so
/// its next operation that needs the runtime stops the old child, confirms it
/// is gone and starts a new one with the new snapshot; a result of an
/// operation begun under the old revision is not published as the new one.
struct AppNetworkApplier {
    ai: Arc<AiOwner>,
    state: &'static network_state::NetworkState,
}

impl AppNetworkApplier {
    /// Returns the AI service's new network revision, when a service exists.
    /// Only the tests need the revision; the application goes through
    /// [`hane_ui::NetworkSettingsApplier::apply_saved`].
    #[cfg(test)]
    fn apply(&self, settings: &hane_network::NetworkSettings) -> Option<u64> {
        self.apply_retiring(settings, None)
    }

    /// `retire` runs once the replaced snapshot is released by every operation.
    fn apply_retiring(
        &self,
        settings: &hane_network::NetworkSettings,
        retire: Option<Box<dyn FnOnce() + Send>>,
    ) -> Option<u64> {
        // The slot lock is held across the snapshot replacement, so a service
        // that starts concurrently reads either the old snapshot and then this
        // revision, or the new snapshot.
        let slot = self.ai.service.slot();
        let context = self.state.replace(settings.clone(), retire);
        slot.as_ref().map(|service| service.apply_network(context))
    }
}

impl hane_ui::NetworkSettingsApplier for AppNetworkApplier {
    fn apply_saved(
        &self,
        settings: &hane_network::NetworkSettings,
        retire: Option<Box<dyn FnOnce() + Send>>,
    ) {
        self.apply_retiring(settings, retire);
    }
}

#[cfg(target_os = "macos")]
pub(crate) type FinderSender = hane_ui::SharedExternalOpenQueue;
#[cfg(not(target_os = "macos"))]
pub(crate) type FinderSender = ();
// Both ends share one queue. The window reads it during the final update
// restart check, so a document that arrives after the restart ticket was
// written keeps the old process running instead of being dropped with it.
#[cfg(target_os = "macos")]
pub(crate) type FinderReceiver = hane_ui::SharedExternalOpenQueue;
#[cfg(not(target_os = "macos"))]
pub(crate) type FinderReceiver = ();

#[cfg(target_os = "macos")]
pub(crate) fn finder_channel() -> (FinderSender, FinderReceiver) {
    let queue = hane_ui::SharedExternalOpenQueue::new();
    (queue.clone(), queue)
}
#[cfg(not(target_os = "macos"))]
pub(crate) fn finder_channel() -> (FinderSender, FinderReceiver) {
    ((), ())
}

/// Creates the application and, on macOS, routes Finder's "Open With" /
/// double-click file URLs (delivered at launch and to a running instance)
/// into `sender`. The platform callback has no `App` access, so the paths are
/// queued for a polling task that opens them through the editor's normal
/// open path. The callback only queues; nothing is opened until the normal
/// editor exists (after the limited child's durable commit, if any). Other
/// platforms ignore `sender`.
pub(crate) fn application_with_finder_open(
    #[cfg_attr(not(target_os = "macos"), allow(unused_variables))] sender: FinderSender,
) -> gpui::Application {
    let application = gpui_platform::application();
    #[cfg(target_os = "macos")]
    application.on_open_urls(move |urls| {
        for path in urls
            .iter()
            .filter_map(|url| finder_open::openable_path_from_url(url))
        {
            // A folder is queued like a document; the window refuses the
            // switch while a session is dirty or saving.
            // A sealed queue means the update helper is already authorized to
            // replace this process: the request cannot be opened here, so it
            // is handed to the process that follows instead of being lost.
            if let Err(path) = sender.try_push(path) {
                hand_off_to_next_process(&path);
            }
        }
    });
    application
}

/// Re-delivers a Finder document that arrived after this process was
/// authorized to exit for an update. A detached shell waits for this process to
/// end and then opens the document with the application bundle, so the
/// replacement process receives it through its ordinary Finder open path
/// (its startup gate waits out the update). Arguments are passed positionally,
/// never interpolated into the script.
#[cfg(target_os = "macos")]
fn hand_off_to_next_process(path: &std::path::Path) {
    let Ok(exe) = std::env::current_exe() else {
        eprintln!("Hane could not hand {} to the updated application.", path.display());
        return;
    };
    let Some(bundle) = exe
        .ancestors()
        .find(|ancestor| ancestor.extension().is_some_and(|ext| ext == "app"))
    else {
        eprintln!("Hane could not hand {} to the updated application.", path.display());
        return;
    };
    let script = r#"while kill -0 "$1" 2>/dev/null; do sleep 0.2; done; exec /usr/bin/open -a "$2" "$3""#;
    if let Err(err) = std::process::Command::new("/bin/sh")
        .arg("-c")
        .arg(script)
        .arg("hane-handoff")
        .arg(std::process::id().to_string())
        .arg(bundle)
        .arg(path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        eprintln!("Hane could not hand {} to the updated application: {err}", path.display());
    }
}

#[cfg(target_os = "windows")]
fn run_context_menu_flag(flag: &std::ffi::OsStr) -> bool {
    if flag == "--register-context-menu" {
        let exe = std::env::current_exe().expect("resolve current exe path");
        hane_ui::context_menu::register(&exe).expect("register Explorer context menu");
        println!("Registered \"Haneで開く\" in Explorer's folder context menu.");
        true
    } else if flag == "--unregister-context-menu" {
        hane_ui::context_menu::unregister().expect("unregister Explorer context menu");
        println!("Removed \"Haneで開く\" from Explorer's folder context menu.");
        true
    } else {
        false
    }
}

fn main() {
    // First of all, before the AI service, state stores, any window or open
    // request: settle a pending update transaction or stop. The guard is the
    // shared runtime lease and lives until the process ends.
    let mut recovered_restart: Option<hane_update::startup::CommittedRestore> = None;
    let _runtime_lease = match update_gate::enter() {
        update_gate::Entry::Start { mut guard, state_root } => {
            // A committed update's first start uses the root the updating
            // application verified; it never falls back to a default of its
            // own. This runs before any store, service or window exists.
            if let Some(root) = state_root
                && !hane_session::pin_state_root(root)
            {
                std::process::exit(70);
            }
            // The limited child may have ended after the durable `Committed`
            // but before it claimed the ticket. The gate hands over that
            // unclaimed ticket (bound to the committed receipt's transaction,
            // install and state root); its state root is pinned here, before
            // any store, service or window exists. The ticket itself is
            // claimed once, only after this start owns the editor.
            if let Some(committed) = guard.as_mut().and_then(|guard| guard.take_committed_restore()) {
                if !hane_session::pin_state_root(committed.restore.state_root().to_path_buf()) {
                    std::process::exit(70);
                }
                recovered_restart = Some(committed);
            }
            guard
        }
        // The helper's limited child: a minimal update window only. Normal
        // startup runs inside it, and only after the helper's durable commit.
        update_gate::Entry::Limited(verified) => {
            limited_start::run(verified);
            return;
        }
        update_gate::Entry::Stop(code) => std::process::exit(code),
    };

    #[cfg(target_os = "windows")]
    if let Some(flag) = std::env::args_os().nth(1)
        && run_context_menu_flag(&flag)
    {
        return;
    }

    // Explorer and CLI path arguments open in the running editor when one is
    // available. Otherwise this launch opens the path without changing the
    // ordinary launch's saved default folder.
    let cli_path = std::env::args_os().nth(1).map(PathBuf::from).map(|path| {
        if path.is_absolute() {
            path
        } else {
            std::env::current_dir().map_or(path.clone(), |cwd| cwd.join(path))
        }
    });
    // The callback is registered on the application before it runs, so a
    // Finder document delivered at launch is queued, not lost.
    let (finder_sender, finder_requests) = finder_channel();
    let has_cli_path = cli_path.is_some();
    // An instance that does not become the editor leaves the ticket unclaimed.
    let Some((mut launch, recovered)) = prepare_launch(cli_path, finder_requests, || {
        recovered_restart.map(|committed| {
            (
                update_restart::restore_after_commit(&committed.restore),
                committed.version,
            )
        })
    }) else {
        return;
    };
    if let Some((mut plan, version)) = recovered {
        // Only a ticket this start claimed announces the committed version,
        // once; an explicit path argument keeps its own folder.
        if plan.is_some() {
            if let (true, Some(plan), Some(path)) = (has_cli_path, plan.as_mut(), launch.path.as_deref()) {
                prioritize_explicit_file(plan, path);
            }
            if !has_cli_path
                && let Some(folder) = plan.as_ref().and_then(|plan| plan.work_folder.clone())
            {
                launch.path = Some(folder);
            }
            launch.restore = plan;
            launch.update_completed = Some(version);
        }
    }
    application_with_finder_open(finder_sender)
        .with_assets(AppAssets)
        .run(move |cx: &mut App| open_editor(cx, launch));
}

/// Everything resolved before the editor window opens. Building it reads the
/// settings store and starts the AI service, so the limited update child
/// builds it only after the helper's commit.
pub(crate) struct EditorLaunch {
    path: Option<PathBuf>,
    #[cfg_attr(feature = "instrument", allow(dead_code))]
    needs_default_prompt: bool,
    ai_service: Option<AiService>,
    update_service: Option<Arc<app_update::AppUpdateService>>,
    /// Saved files of the previous version's window, set only for a process an
    /// update just committed. Opened through the ordinary open path.
    restore: Option<update_restart::RestorePlan>,
    /// The version an update just installed, shown once in Settings. Set only
    /// after the helper's durable commit.
    update_completed: Option<String>,
    #[cfg(target_os = "windows")]
    open_requests: std::sync::mpsc::Receiver<open_request::OpenRequest>,
    #[cfg(target_os = "windows")]
    open_guard: hane_ui::ExternalOpenGuard,
    #[cfg(target_os = "macos")]
    finder_requests: FinderReceiver,
    #[cfg(any(feature = "instrument", feature = "timing-probe"))]
    process_started: std::time::Instant,
    #[cfg(feature = "instrument")]
    config: hane_ui::InstrumentationConfig,
}

/// `None` when this launch must not open an editor (forwarded to a running
/// instance, or the open-request pipe belongs to another instance).
/// `finder_requests` receives Finder documents; it is handed to the editor
/// only here, i.e. never before the update gate / durable commit allowed
/// normal startup.
///
/// `claim` runs only after this launch owns the editor (on Windows, the
/// open-request pipe), so a restart ticket is never consumed by a launch that
/// then fails to become the editor; the ticket stays for the owning instance.
/// It runs at most once and its result is returned with the launch.
pub(crate) fn prepare_launch<T>(
    cli_path: Option<PathBuf>,
    #[cfg_attr(not(target_os = "macos"), allow(unused_variables))] finder_requests: FinderReceiver,
    claim: impl FnOnce() -> T,
) -> Option<(EditorLaunch, T)> {
    #[cfg(target_os = "windows")]
    {
        if let Some(path) = cli_path.as_deref() {
            match open_request::forward(path) {
                Ok(true) => return None,
                Ok(false) => {}
                Err(error) => {
                    eprintln!("Could not forward Explorer open request: {error}");
                    return None;
                }
            }
        }
    }
    #[cfg(target_os = "windows")]
    let open_guard = hane_ui::ExternalOpenGuard::new();
    #[cfg(target_os = "windows")]
    let listened = claim_after_ownership(
        {
            let guard = open_guard.clone();
            move || open_request::listen(guard)
        },
        claim,
    );
    #[cfg(not(target_os = "windows"))]
    let listened = claim_after_ownership(|| Ok::<(), std::convert::Infallible>(()), claim);
    #[cfg_attr(not(target_os = "windows"), allow(unused_variables))]
    let (open_requests, claimed) = match listened {
        Ok(owned) => owned,
        #[cfg(not(target_os = "windows"))]
        Err(never) => match never {},
        #[cfg(target_os = "windows")]
        Err(error) => {
            // Another instance won the pipe-creation race. Forward the path
            // there rather than opening a second editor with the wrong file.
            if let Some(path) = cli_path.as_deref() {
                let _ = open_request::forward(path);
            }
            eprintln!("Could not own Hane's open-request pipe: {error}");
            return None;
        }
    };
    // Without a CLI path, an ordinary launch opens the saved default folder;
    // `needs_default_prompt` is set when there isn't one yet (first run, or
    // the saved folder no longer exists), so the window can prompt for one
    // once it is open.
    #[cfg_attr(feature = "instrument", allow(unused_variables, unused_assignments))]
    let mut needs_default_prompt = false;
    let path = match cli_path {
        Some(cli_path) => Some(cli_path),
        None => {
            let default_folder = StateStores::from_environment()
                .settings()
                .load()
                .default_folder;
            match default_folder {
                Some(folder) if folder.is_dir() => Some(folder),
                _ => {
                    #[cfg_attr(feature = "instrument", allow(unused_assignments))]
                    {
                        needs_default_prompt = true;
                    }
                    None
                }
            }
        }
    };
    #[cfg(any(feature = "instrument", feature = "timing-probe"))]
    let process_started = std::time::Instant::now();
    #[cfg(feature = "instrument")]
    let config = hane_ui::InstrumentationConfig::from_environment();
    // AI is an optional app-owned service. Failure to resolve its state or
    // start its worker must never prevent the Markdown editor from opening.
    // Capture the proxy environment once, before either service exists.
    let _ = network_context();
    let ai_service = start_ai_service();
    let update_service = start_update_service();
    Some((
        EditorLaunch {
            path,
            needs_default_prompt,
            ai_service,
            update_service,
            restore: None,
            update_completed: None,
            #[cfg(target_os = "windows")]
            open_requests,
            #[cfg(target_os = "windows")]
            open_guard,
            #[cfg(target_os = "macos")]
            finder_requests,
            #[cfg(any(feature = "instrument", feature = "timing-probe"))]
            process_started,
            #[cfg(feature = "instrument")]
            config,
        },
        claimed,
    ))
}

/// Makes an explicitly requested file the last restored one, so it is the
/// file shown after the ticket's other files are restored. The other restored
/// files are kept; a directory argument is its own work folder and is left alone.
fn prioritize_explicit_file(plan: &mut update_restart::RestorePlan, explicit: &Path) {
    if explicit.is_dir() {
        return;
    }
    plan.files.retain(|file| file != explicit);
    plan.files.push(explicit.to_path_buf());
}

/// Acquires `ownership` first and runs `claim` only if that succeeded.
fn claim_after_ownership<O, E, T>(
    ownership: impl FnOnce() -> Result<O, E>,
    claim: impl FnOnce() -> T,
) -> Result<(O, T), E> {
    let owned = ownership()?;
    Ok((owned, claim()))
}

/// Opens the editor window and starts the app-owned services inside a running
/// GPUI application.
pub(crate) fn open_editor(cx: &mut App, launch: EditorLaunch) {
    let EditorLaunch {
        path,
        needs_default_prompt,
        ai_service,
        update_service,
        restore,
        update_completed,
        #[cfg(target_os = "windows")]
        open_requests,
        #[cfg(target_os = "windows")]
        open_guard,
        #[cfg(target_os = "macos")]
        finder_requests,
        #[cfg(any(feature = "instrument", feature = "timing-probe"))]
        process_started,
        #[cfg(feature = "instrument")]
        config,
    } = launch;
    #[cfg(feature = "instrument")]
    let _ = needs_default_prompt;
    #[cfg(feature = "instrument")]
    let untitled_source: &str = if config.start_empty {
        ""
    } else {
        DEFAULT_DOCUMENT
    };
    #[cfg(not(feature = "instrument"))]
    let untitled_source: &str = DEFAULT_DOCUMENT;
    // GPUI owns this value for the full application lifetime; views receive
    // handles and may come and go independently.
    let ai_owner = Arc::new(AiOwner::new(ai_service));
    cx.set_global(AppOwnedAiService(Arc::clone(&ai_owner)));
    let ai_service_handle = ai_owner.handle();
    let network_applier: Arc<dyn hane_ui::NetworkSettingsApplier> = Arc::new(AppNetworkApplier {
        ai: Arc::clone(&ai_owner),
        state: network_state(),
    });
    let ai_lifecycle: Arc<dyn AiLifecycle> = ai_owner;
    let update_controller: Option<Arc<dyn AppUpdateController>> = update_service.map(|service| {
        // The service belongs to the application, not to a window.
        cx.set_global(AppOwnedUpdateService(Arc::clone(&service)));
        service as Arc<dyn AppUpdateController>
    });
    register_key_bindings(cx);
    // Finder's right-click "Haneで開く" service feeds the same queue as
    // `on_open_urls`; the application object exists by now.
    #[cfg(target_os = "macos")]
    finder_service::install(finder_requests.clone());
    let bounds = Bounds::centered(None, size(px(960.), px(760.)), cx);
    let window = cx
        .open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |_, cx| {
                cx.new(|cx| {
                    #[cfg_attr(not(feature = "instrument"), allow(unused_mut))]
                    let mut view = match path.as_deref() {
                        Some(path) if path.is_dir() => EditorView::open_work_folder(path, cx),
                        Some(path) => EditorView::open(path, cx).unwrap_or_else(|error| {
                            EditorView::new(
                                &format!("Could not open {}: {error}", path.display()),
                                path.display().to_string(),
                                cx,
                            )
                        }),
                        None => EditorView::new(untitled_source, "Untitled", cx),
                    };
                    view.attach_ai_service(ai_service_handle.clone(), cx);
                    view.attach_ai_lifecycle(Some(ai_lifecycle.clone()));
                    view.attach_app_update(update_controller.clone());
                    view.attach_network_applier(Some(network_applier.clone()));
                    #[cfg(target_os = "macos")]
                    view.attach_external_open_inbox(Some(finder_requests.clone()));
                    #[cfg(target_os = "windows")]
                    view.attach_external_open_guard(open_guard.clone());
                    if let Some(version) = update_completed.as_deref() {
                        // Only a process started by a committed update gets
                        // this; an ordinary launch never shows it.
                        view.show_update_completed(version, cx);
                    }
                    #[cfg(any(feature = "instrument", feature = "timing-probe"))]
                    view.arm_startup_timing(process_started);
                    view
                })
            },
        )
        .expect("open Hane window");
    if let Some(plan) = restore {
        // The saved files of the previous version, through the ordinary open
        // path and in order (the active file last, so it is the one shown).
        // A file that was deleted or moved was left out of the plan and does
        // not stop the others.
        let _ = window.update(cx, |view, _window, cx| {
            view.restore_files(&plan.files, cx);
            if !plan.missing.is_empty() {
                view.report_restore_skipped(plan.missing.len(), cx);
            }
            if plan.explorer_failure.is_some() {
                // After the restore notice so it is not hidden by it.
                view.report_explorer_migration_failed(plan.explorer_repair, cx);
            }
        });
    }
    #[cfg(feature = "instrument")]
    instrument::apply(&window, &config, cx);
    #[cfg(not(feature = "instrument"))]
    {
        window
            .update(cx, |view, window, cx| {
                window.focus(&view.focus_handle(cx), cx);
                // `Context::on_app_quit` (registered inside `EditorView`)
                // only fires on an actual app-quit event, which closing
                // this window does not always raise on its own; flushing
                // here too means an unnamed note's last keystrokes still
                // survive the window simply being closed.
                let view_handle = cx.entity();
                window.on_window_should_close(cx, move |_window, cx| {
                    view_handle.update(cx, |view, _cx| view.flush_pending_drafts());
                    true
                });
                // On macOS the prompt is deferred to the Finder poller
                // below so a document delivered by Finder at launch
                // is not blocked by the folder picker.
                #[cfg(not(target_os = "macos"))]
                if needs_default_prompt {
                    view.prompt_default_work_folder(cx);
                }
            })
            .expect("focus editor");
        cx.activate(true);
    }
    #[cfg(target_os = "macos")]
    cx.spawn(async move |cx| {
        // Finder usually delivers the launch document right after startup;
        // give it a moment before asking for a default folder, and skip the
        // prompt when a document arrived. Documents wait in the queue until
        // the view accepts them; one that arrives while the view is frozen
        // for an update restart check stays queued and is retried.
        let mut prompt_pending = needs_default_prompt && cfg!(not(feature = "instrument"));
        let started = std::time::Instant::now();
        // The queue is shared with the view and a document leaves it only once
        // the view accepted it, so the final restart check can see (and keep
        // the process running for) a document that is still waiting.
        let pending = finder_requests;
        loop {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(40))
                .await;
            if !pending.is_empty() {
                prompt_pending = false;
            }
            if !pending.is_empty()
                && window
                    .update(cx, |view, window, cx| {
                        let accepted = pending.drain_requests_into(|path, protected| {
                            view.submit_external_open_protected(path, protected, cx)
                        });
                        if accepted > 0 {
                            window.activate_window();
                            if !view.settings_is_open() {
                                window.focus(&view.focus_handle(cx), cx);
                            }
                        }
                    })
                    .is_err()
            {
                return;
            }
            if prompt_pending && started.elapsed() >= std::time::Duration::from_millis(500) {
                prompt_pending = false;
                if window
                    .update(cx, |view, _window, cx| view.prompt_default_work_folder(cx))
                    .is_err()
                {
                    return;
                }
            }
        }
    })
    .detach();
    #[cfg(target_os = "windows")]
    cx.spawn(async move |cx| {
        // The sender is acknowledged only once the view really took the path.
        // A request that meets the restart freeze (including the final freeze
        // up to `cx.quit()`) is refused on the pipe, so its sender retries
        // instead of the request dying with this process.
        loop {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(40))
                .await;
            let mut batch = Vec::new();
            while let Ok(request) = open_requests.try_recv() {
                batch.push(request);
            }
            if batch.is_empty() {
                continue;
            }
            if window
                .update(cx, |view, window, cx| {
                    let mut accepted = 0;
                    for request in batch {
                        if request.is_abandoned() {
                            continue;
                        }
                        match view.submit_external_open_protected(
                            request.path(),
                            request.protected(),
                            cx,
                        ) {
                            hane_ui::ExternalOpenOutcome::Accepted => {
                                accepted += 1;
                                request.accept();
                            }
                            hane_ui::ExternalOpenOutcome::Busy => request.refuse(),
                        }
                    }
                    if accepted > 0 {
                        window.activate_window();
                        if !view.settings_is_open() {
                            window.focus(&view.focus_handle(cx), cx);
                        }
                    }
                })
                .is_err()
            {
                return;
            }
        }
    })
    .detach();
}

#[cfg(test)]
mod ownership_claim_tests {
    use super::{claim_after_ownership, prioritize_explicit_file, update_restart};
    use std::cell::Cell;
    use std::path::{Path, PathBuf};

    #[test]
    fn a_lost_ownership_race_leaves_the_ticket_unclaimed() {
        let claims = Cell::new(0);
        let outcome: Result<((), u8), &str> = claim_after_ownership(
            || Err("another instance owns the open-request pipe"),
            || {
                claims.set(claims.get() + 1);
                1
            },
        );
        assert!(outcome.is_err());
        assert_eq!(claims.get(), 0, "the ticket stays for the owning instance");
    }

    #[test]
    fn an_explicit_file_is_restored_last_over_the_ticket_files() {
        let mut plan = update_restart::RestorePlan {
            work_folder: None,
            files: vec![PathBuf::from("/nope/a.md"), PathBuf::from("/nope/b.md")],
            missing: Vec::new(),
            explorer_failure: None,
            explorer_repair: Default::default(),
        };
        prioritize_explicit_file(&mut plan, Path::new("/nope/a.md"));
        assert_eq!(
            plan.files,
            vec![PathBuf::from("/nope/b.md"), PathBuf::from("/nope/a.md")],
            "the explicit file is last (shown); other restored files are kept"
        );
        prioritize_explicit_file(&mut plan, Path::new("/nope/c.md"));
        assert_eq!(plan.files.last(), Some(&PathBuf::from("/nope/c.md")));
        assert_eq!(plan.files.len(), 3);
    }

    #[test]
    fn the_ticket_is_claimed_exactly_once_after_ownership() {
        let order = Cell::new(0);
        let claims = Cell::new(0);
        let outcome: Result<(u8, u8), &str> = claim_after_ownership(
            || {
                assert_eq!(order.get(), 0, "ownership comes first");
                order.set(1);
                Ok(7)
            },
            || {
                assert_eq!(order.get(), 1, "the claim follows ownership");
                claims.set(claims.get() + 1);
                9
            },
        );
        assert_eq!(outcome, Ok((7, 9)));
        assert_eq!(claims.get(), 1);
    }
}

#[cfg(test)]
mod network_applier_tests {
    use super::{AiOwner, AppNetworkApplier};
    use crate::network_state::NetworkState;
    use hane_ai::{
        AiService, AiServiceConfig, FakeCredentialStore, ShellEnvironmentPolicyFormat,
        SystemBrowserOpener,
    };
    use hane_network::{EnvPlatform, NetworkSettings, ProxyEnvironment, ResolvedRoute, Scheme};
    use hane_update::testing::TempDir;
    use std::path::PathBuf;
    use std::sync::Arc;

    fn state() -> &'static NetworkState {
        Box::leak(Box::new(NetworkState::new(
            NetworkSettings::default(),
            ProxyEnvironment::from_vars(
                EnvPlatform::MacOs,
                [("https_proxy", "http://proxy.example:3128")],
            ),
            Arc::new(hane_ui::OsProxyCredentialStore),
        )))
    }

    #[test]
    fn a_saved_setting_advances_the_ai_revision_and_the_snapshot_without_a_restart() {
        let root = TempDir::new("network-applier");
        let service = AiService::spawn(AiServiceConfig {
            app_data_root: root.path().to_path_buf(),
            binary_path: PathBuf::new(),
            binary_resolution_error: None,
            credential_store: Arc::new(FakeCredentialStore::new()),
            browser_opener: Arc::new(SystemBrowserOpener),
            shell_env_format: ShellEnvironmentPolicyFormat::Filters,
        })
        .unwrap();
        let state = state();
        let applier = AppNetworkApplier {
            ai: Arc::new(AiOwner::new(Some(service))),
            state,
        };
        let before = state.current();

        assert_eq!(applier.apply(&NetworkSettings::direct()), Some(1));
        assert_eq!(applier.apply(&NetworkSettings::default()), Some(2));
        assert_eq!(applier.apply(&NetworkSettings::direct()), Some(3));

        assert_eq!(
            state.current().route(Scheme::Https, "api.github.com").unwrap(),
            ResolvedRoute::Direct
        );
        // The snapshot taken before the save is unchanged.
        assert!(matches!(
            before.route(Scheme::Https, "api.github.com").unwrap(),
            ResolvedRoute::HttpProxy { .. }
        ));
    }

    #[test]
    fn a_save_without_an_ai_service_still_replaces_the_snapshot() {
        let state = state();
        let applier = AppNetworkApplier {
            ai: Arc::new(AiOwner::new(None)),
            state,
        };
        assert_eq!(applier.apply(&NetworkSettings::direct()), None);
        assert_eq!(*state.current().settings(), NetworkSettings::direct());
    }
}

#[cfg(test)]
mod service_slot_tests {
    use super::{AiResume, ServiceSlot};
    use std::sync::mpsc::{Receiver, Sender, channel};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    /// Drop blocks until released, like a worker still finishing a runtime.
    struct Blocking(Option<Mutex<Receiver<()>>>);

    impl Drop for Blocking {
        fn drop(&mut self) {
            if let Some(gate) = self.0.take() {
                let _ = gate.lock().unwrap().recv();
            }
        }
    }

    fn blocking() -> (Blocking, Sender<()>) {
        let (release, gate) = channel();
        (Blocking(Some(Mutex::new(gate))), release)
    }

    #[test]
    fn resume_after_a_timed_out_stop_does_not_start_a_second_service() {
        let (service, release) = blocking();
        let slot = ServiceSlot::new(Some(service));
        assert!(slot.stop(Duration::from_millis(20)).is_err());

        let starts = Arc::new(Mutex::new(0));
        let counter = Arc::clone(&starts);
        let resumed = slot.resume(
            move || {
                *counter.lock().unwrap() += 1;
                Some(Blocking(None))
            },
            |_| (),
        );
        assert!(matches!(resumed, AiResume::Stopping));
        assert_eq!(*starts.lock().unwrap(), 0);
        assert!(slot.stop(Duration::from_millis(20)).is_err());

        // Once the old service is really gone, the pending start runs once.
        release.send(()).unwrap();
        let mut resumed = AiResume::Stopping;
        for _ in 0..200 {
            resumed = slot.poll_resume(|_| ());
            if !matches!(resumed, AiResume::Stopping) {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(matches!(resumed, AiResume::Started(())));
        assert_eq!(*starts.lock().unwrap(), 1);
    }

    #[test]
    fn a_resume_kept_during_a_slow_stop_starts_once_after_the_stop_completes() {
        let (service, release) = blocking();
        let slot = ServiceSlot::new(Some(service));
        // A stop past its timeout, as when the 10s limit is exceeded.
        assert!(slot.stop(Duration::from_millis(20)).is_err());

        let starts = Arc::new(Mutex::new(0));
        let counter = Arc::clone(&starts);
        assert!(matches!(
            slot.resume(
                move || {
                    *counter.lock().unwrap() += 1;
                    Some(Blocking(None))
                },
                |_| (),
            ),
            AiResume::Stopping
        ));
        assert_eq!(*starts.lock().unwrap(), 0, "no second service while stopping");

        release.send(()).unwrap();
        let mut resumed = AiResume::Stopping;
        for _ in 0..200 {
            resumed = slot.poll_resume(|_| ());
            if !matches!(resumed, AiResume::Stopping) {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            matches!(resumed, AiResume::Started(())),
            "the AI must not stay unavailable"
        );
        assert_eq!(*starts.lock().unwrap(), 1);
        assert!(matches!(
            slot.resume(|| panic!("already running"), |_| ()),
            AiResume::Started(())
        ));
        assert_eq!(*starts.lock().unwrap(), 1);
    }

    #[test]
    fn resume_after_a_completed_stop_starts_the_service() {
        let slot = ServiceSlot::new(Some(Blocking(None)));
        assert!(slot.stop(Duration::from_secs(5)).is_ok());
        let resumed = slot.resume(|| Some(Blocking(None)), |_| ());
        assert!(matches!(resumed, AiResume::Started(())));
    }

    #[test]
    fn an_immediate_start_failure_is_reported_and_a_later_resume_retries() {
        let slot = ServiceSlot::new(Some(Blocking(None)));
        assert!(slot.stop(Duration::from_secs(5)).is_ok());
        let starts = Arc::new(Mutex::new(0));
        let counter = Arc::clone(&starts);
        let failed = slot.resume(
            move || {
                *counter.lock().unwrap() += 1;
                None::<Blocking>
            },
            |_| (),
        );
        assert!(matches!(failed, AiResume::Failed));
        // Polling only looks; it never starts again.
        assert!(matches!(slot.poll_resume(|_| ()), AiResume::Failed));
        assert_eq!(*starts.lock().unwrap(), 1);
        // A separate explicit resume may retry.
        assert!(matches!(
            slot.resume(|| Some(Blocking(None)), |_| ()),
            AiResume::Started(())
        ));
    }

    #[test]
    fn a_failed_pending_start_ends_the_wait_without_repeating_the_start() {
        let (service, release) = blocking();
        let slot = ServiceSlot::new(Some(service));
        assert!(slot.stop(Duration::from_millis(20)).is_err());
        let starts = Arc::new(Mutex::new(0));
        let counter = Arc::clone(&starts);
        assert!(matches!(
            slot.resume(
                move || {
                    *counter.lock().unwrap() += 1;
                    None::<Blocking>
                },
                |_| (),
            ),
            AiResume::Stopping
        ));

        release.send(()).unwrap();
        let mut outcome = AiResume::Stopping;
        for _ in 0..200 {
            outcome = slot.poll_resume(|_| ());
            if !matches!(outcome, AiResume::Stopping) {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(matches!(outcome, AiResume::Failed));
        for _ in 0..5 {
            assert!(matches!(slot.poll_resume(|_| ()), AiResume::Failed));
        }
        assert_eq!(*starts.lock().unwrap(), 1, "polling never restarts");

        // A new explicit resume after the failure can try again.
        assert!(matches!(
            slot.resume(|| Some(Blocking(None)), |_| ()),
            AiResume::Started(())
        ));
    }
}
