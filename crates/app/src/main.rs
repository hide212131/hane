#[cfg(not(feature = "instrument"))]
use gpui::Focusable;
use gpui::{App, AppContext, Application, Bounds, WindowBounds, WindowOptions, px, size};
use hane_ui::{EditorView, register_key_bindings};
use std::path::PathBuf;

#[cfg(feature = "instrument")]
mod instrument;

const DEFAULT_DOCUMENT: &str = "# Hane Phase 4\n\n日本語IME、範囲選択、Undo / Redo、Markdown記号の段階表示に加えて、画像、表、保存、自動保存、Recent Files、themeを試せます。\n\n![Hane feather](assets/phase4-feather.svg)\n\n| Feature | Status |\n|:---|---:|\n| Typora-style editing | ✓ |\n| Atomic autosave | ✓ |\n| Light / Dark theme | ✓ |\n\n## Polish\n\n画像と表も元Markdownを唯一の正として保持します。行へカーソルを移動するとsourceを編集できます。\n";

fn main() {
    let path = std::env::args_os().nth(1).map(PathBuf::from);
    #[cfg(feature = "instrument")]
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
    Application::new().run(move |cx: &mut App| {
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
                            Some(path) => EditorView::open(path, cx).unwrap_or_else(|error| {
                                EditorView::new(
                                    &format!("Could not open {}: {error}", path.display()),
                                    path.display().to_string(),
                                    cx,
                                )
                            }),
                            None => EditorView::new(untitled_source, "Untitled", cx),
                        };
                        #[cfg(feature = "instrument")]
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
                })
                .expect("focus editor");
            cx.activate(true);
        }
    });
}
