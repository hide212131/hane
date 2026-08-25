use gpui::{
    App, AppContext, Application, Bounds, Focusable, WindowBounds, WindowOptions, px, size,
};
use hane_ui::{EditorView, register_key_bindings};
use std::path::PathBuf;
use std::time::Instant;

fn main() {
    let process_started = Instant::now();
    let path = std::env::args_os().nth(1).map(PathBuf::from);
    #[cfg(debug_assertions)]
    let development_cursor_offset = std::env::var("HANE_DEV_CURSOR_OFFSET").ok().map(|value| {
        value
            .parse::<usize>()
            .expect("HANE_DEV_CURSOR_OFFSET must be a byte offset")
    });
    #[cfg(debug_assertions)]
    let development_cursor_down = std::env::var("HANE_DEV_CURSOR_DOWN").ok().map(|value| {
        value
            .parse::<usize>()
            .expect("HANE_DEV_CURSOR_DOWN must be a non-negative integer")
    });
    Application::new().run(move |cx: &mut App| {
        register_key_bindings(cx);
        let bounds = Bounds::centered(None, size(px(960.), px(760.)), cx);
        let file_open_started = Instant::now();
        let window = cx.open_window(WindowOptions { window_bounds: Some(WindowBounds::Windowed(bounds)), ..Default::default() }, |_, cx| {
            cx.new(|cx| match path.as_deref() {
                Some(path) => EditorView::open(path, cx).unwrap_or_else(|error| EditorView::new(&format!("Could not open {}: {error}", path.display()), path.display().to_string(), cx)),
                None => EditorView::new("# Hane Phase 0\n\n日本語 IME と **太字表示** を試せます。\n\n10 MB / 100 MB fixture は `cargo run -p hane-benchmark --bin hane-bench -- fixtures` で生成できます。\n", "Untitled", cx),
            })
        }).expect("open Hane window");
        #[cfg(debug_assertions)]
        if let Some(offset) = development_cursor_offset {
            window
                .update(cx, |view, _, cx| {
                    view.set_cursor_offset_for_development(offset, cx)
                })
                .expect("set development cursor")
                .expect("HANE_DEV_CURSOR_OFFSET must be a valid character boundary");
        }
        #[cfg(debug_assertions)]
        if let Some(count) = development_cursor_down {
            window
                .update(cx, |view, _, cx| {
                    view.move_cursor_down_for_development(count, cx)
                })
                .expect("move development cursor down")
                .expect("development cursor movement must succeed");
        }
        window.update(cx, |view, window, cx| { window.focus(&view.focus_handle(cx)); }).expect("focus editor");
        cx.activate(true);
        eprintln!(
            "hane_ready startup_time_ms={:.3} file_open_time_ms={:.3}",
            process_started.elapsed().as_secs_f64() * 1000.0,
            file_open_started.elapsed().as_secs_f64() * 1000.0
        );
    });
}
