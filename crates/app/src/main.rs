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
use hane_ui::{AppAssets, AppUpdateController, EditorView, register_key_bindings};
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
#[cfg(target_os = "windows")]
mod open_request;

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
    // Interim identity until the OS adapter (a later stage) supplies the real
    // volume and owner identity; nothing is applied from it in this stage.
    let install_id = InstallId::derive(&InstallFacts {
        os: std::env::consts::OS,
        volume_id: &volume,
        install_path: &install_dir.to_string_lossy(),
        owner: &owner,
    })
    .ok()?;
    let updates_root = state.root().join("updates").join(install_id.as_str());
    let receipts_dir = install_dir
        .join(install_id.coordination_dir_name())
        .join("receipts");
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

#[cfg(target_os = "macos")]
type FinderSender = std::sync::mpsc::Sender<PathBuf>;
#[cfg(not(target_os = "macos"))]
type FinderSender = ();

/// Creates the application and, on macOS, routes Finder's "Open With" /
/// double-click file URLs (delivered at launch and to a running instance)
/// into `sender`. The platform callback has no `App` access, so the paths are
/// queued for a polling task that opens them through the editor's normal
/// open path. Other platforms ignore `sender`.
fn application_with_finder_open(
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
    #[cfg(target_os = "windows")]
    {
        if let Some(path) = cli_path.as_deref() {
            match open_request::forward(path) {
                Ok(true) => return,
                Ok(false) => {}
                Err(error) => {
                    eprintln!("Could not forward Explorer open request: {error}");
                    return;
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
            return;
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
    #[cfg(feature = "instrument")]
    let untitled_source: &str = if config.start_empty {
        ""
    } else {
        DEFAULT_DOCUMENT
    };
    #[cfg(not(feature = "instrument"))]
    let untitled_source: &str = DEFAULT_DOCUMENT;
    #[cfg(target_os = "macos")]
    let (finder_sender, finder_requests) = std::sync::mpsc::channel::<PathBuf>();
    #[cfg(not(target_os = "macos"))]
    let finder_sender = ();
    application_with_finder_open(finder_sender)
        .with_assets(AppAssets)
        .run(move |cx: &mut App| {
            if let Some(service) = ai_service {
                // GPUI owns this value for the full application lifetime;
                // views receive handles and may come and go independently.
                cx.set_global(AppOwnedAiService(service));
            }
            let ai_service_handle = cx
                .try_global::<AppOwnedAiService>()
                .map(|owner| owner.0.handle());
            let update_controller: Option<Arc<dyn AppUpdateController>> =
                update_service.map(|service| {
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
                                Some(path) if path.is_dir() => {
                                    EditorView::open_work_folder(path, cx)
                                }
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
                // Finder usually delivers the launch document right after
                // startup; give it a moment before asking for a default
                // folder, and skip the prompt when a document arrived.
                let mut prompt_pending =
                    needs_default_prompt && cfg!(not(feature = "instrument"));
                let started = std::time::Instant::now();
                loop {
                    cx.background_executor()
                        .timer(std::time::Duration::from_millis(40))
                        .await;
                    while let Ok(path) = finder_requests.try_recv() {
                        prompt_pending = false;
                        // A folder is never opened from Finder: that would
                        // replace the sessions without the dirty-file guard
                        // `open_path` applies to documents.
                        if path.is_dir() {
                            continue;
                        }
                        if window
                            .update(cx, |view, window, cx| {
                                view.open_external_path(&path, cx);
                                window.activate_window();
                                if !view.settings_is_open() {
                                    window.focus(&view.focus_handle(cx), cx);
                                }
                            })
                            .is_err()
                        {
                            return;
                        }
                    }
                    if prompt_pending && started.elapsed() >= std::time::Duration::from_millis(500)
                    {
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
                }
            })
            .detach();
        });
}
