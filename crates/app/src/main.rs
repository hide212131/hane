#![cfg_attr(
    all(target_os = "windows", not(feature = "instrument")),
    windows_subsystem = "windows"
)]

#[cfg(any(target_os = "windows", not(feature = "instrument")))]
use gpui::Focusable;
use gpui::{App, AppContext, Bounds, Global, WindowBounds, WindowOptions, px, size};
use hane_ai::{
    AiService, AiServiceConfig, EXPLICIT_OVERRIDE_ENV_VAR, OsCredentialStore,
    ShellEnvironmentPolicyFormat, SystemBrowserOpener, resolve_runtime_binary,
};
use hane_session::{FileStateStore, StateStores};
use hane_ui::{AppAssets, AppUpdateController, EditorView, register_key_bindings};
use hane_update::clock::SystemClock;
use hane_update::identity::{InstallFacts, InstallId};
use hane_update::model::Target;
use hane_update::trust::TrustPolicy;
use std::path::{Path, PathBuf};
use std::sync::Arc;

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

struct AppOwnedAiService(AiService);

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
    let (install_id, receipts_dir) = match hane_update::platform::locate(&exe) {
        Ok(placement) => {
            let receipts = hane_update::platform::lease::Coordination::receipts_dir_for(
                &placement.coordination_parent,
                &placement.install_id,
            );
            (placement.install_id, receipts)
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
                install_path: &install_dir.to_string_lossy(),
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
    Some(Arc::new(app_update::AppUpdateService::new(
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
    )))
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
    let _runtime_lease = match update_gate::enter() {
        update_gate::Entry::Start { guard, state_root } => {
            // A committed update's first start uses the root the updating
            // application verified; it never falls back to a default of its
            // own. This runs before any store, service or window exists.
            if let Some(root) = state_root
                && !hane_session::pin_state_root(root)
            {
                std::process::exit(70);
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
    let Some(launch) = prepare_launch(cli_path) else {
        return;
    };
    gpui_platform::application()
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
    #[cfg(target_os = "windows")]
    open_requests: std::sync::mpsc::Receiver<PathBuf>,
    #[cfg(any(feature = "instrument", feature = "timing-probe"))]
    process_started: std::time::Instant,
    #[cfg(feature = "instrument")]
    config: hane_ui::InstrumentationConfig,
}

/// `None` when this launch must not open an editor (forwarded to a running
/// instance, or the open-request pipe belongs to another instance).
pub(crate) fn prepare_launch(cli_path: Option<PathBuf>) -> Option<EditorLaunch> {
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
    let open_requests = match open_request::listen() {
        Ok(receiver) => receiver,
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
    Some(EditorLaunch {
        path,
        needs_default_prompt,
        ai_service,
        update_service,
        #[cfg(target_os = "windows")]
        open_requests,
        #[cfg(any(feature = "instrument", feature = "timing-probe"))]
        process_started,
        #[cfg(feature = "instrument")]
        config,
    })
}

/// Opens the editor window and starts the app-owned services inside a running
/// GPUI application.
pub(crate) fn open_editor(cx: &mut App, launch: EditorLaunch) {
    let EditorLaunch {
        path,
        needs_default_prompt,
        ai_service,
        update_service,
        #[cfg(target_os = "windows")]
        open_requests,
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
    if let Some(service) = ai_service {
        // GPUI owns this value for the full application lifetime;
        // views receive handles and may come and go independently.
        cx.set_global(AppOwnedAiService(service));
    }
    let ai_service_handle = cx
        .try_global::<AppOwnedAiService>()
        .map(|owner| owner.0.handle());
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
                    view.attach_app_update(update_controller.clone());
                    #[cfg(any(feature = "instrument", feature = "timing-probe"))]
                    view.arm_startup_timing(process_started);
                    view
                })
            },
        )
        .expect("open Hane window");
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
                if needs_default_prompt {
                    view.prompt_default_work_folder(cx);
                }
            })
            .expect("focus editor");
        cx.activate(true);
    }
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
