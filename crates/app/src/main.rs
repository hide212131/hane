#![cfg_attr(
    all(target_os = "windows", not(feature = "instrument")),
    windows_subsystem = "windows"
)]

#[cfg(not(feature = "instrument"))]
use gpui::Focusable;
use gpui::{App, AppContext, Application, Bounds, WindowBounds, WindowOptions, px, size};
use hane_session::StateStores;
use hane_ui::{EditorView, WorkFolderIcons, register_key_bindings};
use std::path::PathBuf;

#[cfg(target_os = "windows")]
mod context_menu;

#[cfg(feature = "instrument")]
mod instrument;

const DEFAULT_DOCUMENT: &str = "# Hane Phase 4\n\n日本語IME、範囲選択、Undo / Redo、Markdown記号の段階表示に加えて、画像、表、保存、自動保存、Recent Files、themeを試せます。\n\n![Hane feather](assets/phase4-feather.svg)\n\n| Feature | Status |\n|:---|---:|\n| Typora-style editing | ✓ |\n| Atomic autosave | ✓ |\n| Light / Dark theme | ✓ |\n\n## Polish\n\n画像と表も元Markdownを唯一の正として保持します。行へカーソルを移動するとsourceを編集できます。\n";

#[cfg(target_os = "windows")]
fn run_context_menu_flag(flag: &std::ffi::OsStr) -> bool {
    if flag == "--register-context-menu" {
        let exe = std::env::current_exe().expect("resolve current exe path");
        context_menu::register(&exe).expect("register Explorer context menu");
        println!("Registered \"Haneで開く\" in Explorer's folder context menu.");
        true
    } else if flag == "--unregister-context-menu" {
        context_menu::unregister().expect("unregister Explorer context menu");
        println!("Removed \"Haneで開く\" from Explorer's folder context menu.");
        true
    } else {
        false
    }
}

fn main() {
    #[cfg(target_os = "windows")]
    if let Some(flag) = std::env::args_os().nth(1) {
        if run_context_menu_flag(&flag) {
            return;
        }
    }

    // A path argument (typed manually, or supplied by Explorer's "Open with
    // Hane") opens that folder for this launch only; it deliberately bypasses
    // `default_folder` on both ends, so it neither reads nor overwrites the
    // folder an ordinary launch opens.
    let cli_path = std::env::args_os().nth(1).map(PathBuf::from);
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
    #[cfg(feature = "instrument")]
    let untitled_source: &str = if config.start_empty {
        ""
    } else {
        DEFAULT_DOCUMENT
    };
    #[cfg(not(feature = "instrument"))]
    let untitled_source: &str = DEFAULT_DOCUMENT;
    Application::new()
        .with_assets(WorkFolderIcons)
        .run(move |cx: &mut App| {
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
                        window.focus(&view.focus_handle(cx));
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
        });
}
