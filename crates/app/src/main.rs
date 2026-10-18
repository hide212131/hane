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
use hane_ui::{AiLifecycle, AppAssets, AppUpdateController, EditorView, register_key_bindings};
use hane_update::clock::SystemClock;
use hane_update::identity::{InstallFacts, InstallId};
use hane_update::model::Target;
use hane_update::trust::TrustPolicy;
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[cfg(any(target_os = "macos", all(test, unix)))]
mod finder_open;

mod app_update;
mod https_transport;
mod limited_start;
#[cfg(target_os = "windows")]
mod open_request;
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
    /// kept and run once by the stop thread when the old service is gone, and
    /// a later `resume` then returns the handle of that service.
    fn resume<H>(
        &self,
        start: impl FnOnce() -> Option<S> + Send + 'static,
        handle: impl Fn(&S) -> H,
    ) -> Option<H> {
        use std::sync::atomic::Ordering;
        let mut slot = self.slot();
        if self.stopping.load(Ordering::SeqCst) {
            let mut pending = lock_ignoring_poison(&self.pending_start);
            if pending.is_none() {
                *pending = Some(Box::new(start));
            }
            return None;
        }
        if slot.is_none() {
            *slot = start();
        }
        slot.as_ref().map(handle)
    }
}

impl AiLifecycle for AiOwner {
    fn stop(&self, timeout: std::time::Duration) -> Result<(), String> {
        self.service.stop(timeout)
    }

    fn resume(&self) -> Option<hane_ai::AiServiceHandle> {
        self.service.resume(start_ai_service, AiService::handle)
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
    let located = hane_update::platform::locate(&exe).ok();
    let (install_id, receipts_dir) = match &located {
        Some(placement) => {
            let receipts = hane_update::platform::lease::Coordination::receipts_dir_for(
                &placement.coordination_parent,
                &placement.install_id,
            );
            (placement.install_id.clone(), receipts)
        }
        None => {
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
        Arc::new(https_transport::LazyHttpsTransport::default()),
        Arc::new(SystemClock::new()),
    ));
    // Only a supported placement can hand a Ready update to the helper, and
    // even then "restart and update" is offered only once the trust policy is
    // configured; the production policy is not, so it stays fail-closed.
    if let Some(placement) = located {
        service.enable_restart(app_update::RestartWiring {
            placement,
            exe,
            state_root: state.root().to_path_buf(),
        });
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
    AiService::spawn(AiServiceConfig {
        app_data_root: state.root().to_path_buf(),
        binary_path,
        binary_resolution_error,
        credential_store: Arc::new(OsCredentialStore::new("com.hide212131.hane.ai")),
        browser_opener: Arc::new(SystemBrowserOpener),
        shell_env_format: ShellEnvironmentPolicyFormat::Filters,
    })
    .ok()
}

#[cfg(target_os = "macos")]
pub(crate) type FinderSender = std::sync::mpsc::Sender<PathBuf>;
#[cfg(not(target_os = "macos"))]
pub(crate) type FinderSender = ();
#[cfg(target_os = "macos")]
pub(crate) type FinderReceiver = std::sync::mpsc::Receiver<PathBuf>;
#[cfg(not(target_os = "macos"))]
pub(crate) type FinderReceiver = ();

#[cfg(target_os = "macos")]
pub(crate) fn finder_channel() -> (FinderSender, FinderReceiver) {
    std::sync::mpsc::channel::<PathBuf>()
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
            .filter_map(|url| finder_open::markdown_path_from_url(url))
        {
            let _ = sender.send(path);
        }
    });
    application
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
    if let Some((plan, version)) = recovered {
        // Only a ticket this start claimed announces the committed version,
        // once; an explicit path argument keeps its own folder.
        if plan.is_some() {
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
    open_requests: std::sync::mpsc::Receiver<PathBuf>,
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
    let listened = claim_after_ownership(open_request::listen, claim);
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
    let ai_lifecycle: Arc<dyn AiLifecycle> = ai_owner;
    let update_controller: Option<Arc<dyn AppUpdateController>> = update_service.map(|service| {
        // The service belongs to the application, not to a window.
        cx.set_global(AppOwnedUpdateService(Arc::clone(&service)));
        service as Arc<dyn AppUpdateController>
    });
    register_key_bindings(cx);
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
        let mut pending = hane_ui::ExternalOpenQueue::new();
        loop {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(40))
                .await;
            while let Ok(path) = finder_requests.try_recv() {
                prompt_pending = false;
                // A folder is never opened from Finder: that would replace
                // the sessions without the dirty-file guard `open_path`
                // applies to documents.
                if path.is_dir() {
                    continue;
                }
                pending.push(path);
            }
            if !pending.is_empty()
                && window
                    .update(cx, |view, window, cx| {
                        let accepted =
                            pending.drain_into(|path| view.submit_external_open(path, cx));
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
        // Paths wait here until the view accepts them; a request that
        // arrives while the view is frozen for an update restart
        // check stays queued and is retried on the next tick instead
        // of being consumed.
        let mut pending = hane_ui::ExternalOpenQueue::new();
        loop {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(40))
                .await;
            while let Ok(path) = open_requests.try_recv() {
                pending.push(path);
            }
            if pending.is_empty() {
                continue;
            }
            if window
                .update(cx, |view, window, cx| {
                    let accepted = pending.drain_into(|path| view.submit_external_open(path, cx));
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
    use super::claim_after_ownership;
    use std::cell::Cell;

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
mod service_slot_tests {
    use super::ServiceSlot;
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
        assert!(resumed.is_none());
        assert_eq!(*starts.lock().unwrap(), 0);
        assert!(slot.stop(Duration::from_millis(20)).is_err());

        // Once the old service is really gone, resuming starts a new one.
        release.send(()).unwrap();
        let mut resumed = None;
        for _ in 0..200 {
            let counter = Arc::clone(&starts);
            resumed = slot.resume(
                move || {
                    *counter.lock().unwrap() += 1;
                    Some(Blocking(None))
                },
                |_| (),
            );
            if resumed.is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(resumed.is_some());
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
        assert!(
            slot.resume(
                move || {
                    *counter.lock().unwrap() += 1;
                    Some(Blocking(None))
                },
                |_| (),
            )
            .is_none()
        );
        assert_eq!(*starts.lock().unwrap(), 0, "no second service while stopping");

        release.send(()).unwrap();
        let mut resumed = None;
        for _ in 0..200 {
            resumed = slot.resume(|| panic!("the pending start must be the only one"), |_| ());
            if resumed.is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(resumed.is_some(), "the AI must not stay unavailable");
        assert_eq!(*starts.lock().unwrap(), 1);
        assert!(slot.resume(|| panic!("already running"), |_| ()).is_some());
        assert_eq!(*starts.lock().unwrap(), 1);
    }

    #[test]
    fn resume_after_a_completed_stop_starts_the_service() {
        let slot = ServiceSlot::new(Some(Blocking(None)));
        assert!(slot.stop(Duration::from_secs(5)).is_ok());
        let resumed = slot.resume(|| Some(Blocking(None)), |_| ());
        assert!(resumed.is_some());
    }
}
