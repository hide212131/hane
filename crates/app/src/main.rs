use gpui::{
    App, AppContext, Application, Bounds, Focusable, Timer, WindowBounds, WindowOptions, px, size,
};
use hane_benchmark::process_memory_bytes;
use hane_document::SourceRange;
use hane_presentation::present_bold;
use hane_ui::{EditorView, register_key_bindings};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

fn main() {
    let process_started = Instant::now();
    let path = std::env::args_os().nth(1).map(PathBuf::from);
    let measurement_cursor_offset = std::env::var("HANE_MEASUREMENT_CURSOR_OFFSET")
        .ok()
        .filter(|value| !value.is_empty())
        .map(|value| {
            value
                .parse::<usize>()
                .expect("HANE_MEASUREMENT_CURSOR_OFFSET must be a byte offset")
        });
    let development_cursor_down = std::env::var("HANE_DEV_CURSOR_DOWN").ok().map(|value| {
        value
            .parse::<usize>()
            .expect("HANE_DEV_CURSOR_DOWN must be a non-negative integer")
    });
    Application::new().run(move |cx: &mut App| {
        register_key_bindings(cx);
        let bounds = Bounds::centered(None, size(px(960.), px(760.)), cx);
        let window = cx.open_window(WindowOptions { window_bounds: Some(WindowBounds::Windowed(bounds)), ..Default::default() }, |_, cx| {
            cx.new(|cx| {
                let mut view = match path.as_deref() {
                    Some(path) => EditorView::open(path, cx).unwrap_or_else(|error| EditorView::new(&format!("Could not open {}: {error}", path.display()), path.display().to_string(), cx)),
                    None if std::env::var("HANE_MEASUREMENT_EMPTY")
                        .is_ok_and(|value| !value.is_empty()) => EditorView::new("", "Untitled", cx),
                    None => EditorView::new("# Hane Phase 4\n\n日本語IME、範囲選択、Undo / Redo、Markdown記号の段階表示に加えて、画像、表、保存、自動保存、Recent Files、themeを試せます。\n\n![Hane feather](assets/phase4-feather.svg)\n\n| Feature | Status |\n|:---|---:|\n| Typora-style editing | ✓ |\n| Atomic autosave | ✓ |\n| Light / Dark theme | ✓ |\n\n## Polish\n\n画像と表も元Markdownを唯一の正として保持します。行へカーソルを移動するとsourceを編集できます。\n", "Untitled", cx),
                };
                view.arm_startup_timing(process_started);
                view
            })
        }).expect("open Hane window");
        if let Some(offset) = measurement_cursor_offset {
            window
                .update(cx, |view, _, cx| {
                    view.set_cursor_offset_for_measurement(offset, cx)
                })
                .expect("set development cursor")
                .expect("HANE_MEASUREMENT_CURSOR_OFFSET must be a valid character boundary");
        }
        if let Some(count) = development_cursor_down {
            window
                .update(cx, |view, _, cx| {
                    view.move_cursor_down_for_development(count, cx)
                })
                .expect("move development cursor down")
                .expect("development cursor movement must succeed");
        }
        let no_focus = std::env::var("HANE_PHASE0_NO_FOCUS").is_ok_and(|value| !value.is_empty());
        if !no_focus {
            window.update(cx, |view, window, cx| {
                window.focus(&view.focus_handle(cx));
            }).expect("focus editor");
        }
        if std::env::var("HANE_PHASE2_AUTOSCROLL")
            .or_else(|_| std::env::var("HANE_PHASE1_AUTOSCROLL"))
            .or_else(|_| std::env::var("HANE_PHASE0_AUTOSCROLL"))
            .is_ok_and(|value| !value.is_empty())
        {
            window
                .update(cx, |view, _, cx| {
                    view.enable_display_linked_scroll_measurement();
                    cx.notify();
                })
                .expect("schedule display-linked scroll measurement");
        }
        if std::env::var("HANE_MEASURE_IDLE_RSS").is_ok_and(|value| !value.is_empty()) {
            let view = window.entity(cx).expect("read Hane root entity").downgrade();
            cx.spawn(async move |cx| {
                Timer::after(std::time::Duration::from_secs(30)).await;
                let rss = process_memory_bytes();
                let _ = view.update(cx, |view, _| view.record_phase0_idle_memory(rss));
            }).detach();
        }
        if std::env::var("HANE_PHASE0_BACKGROUND_PRESENTATION")
            .is_ok_and(|value| !value.is_empty())
        {
            let view = window.entity(cx).expect("read Hane root entity").downgrade();
            cx.spawn(async move |cx| {
                let source: Arc<str> =
                    ("background **presentation update** 日本語 🙂\n").repeat(16_384).into();
                let range = SourceRange::new(0, source.len());
                for generation in 1_u64.. {
                    let source = Arc::clone(&source);
                    cx.background_executor().spawn(async move {
                        std::hint::black_box(present_bold(
                            generation,
                            hane_document::Revision(generation),
                            range,
                            &source,
                        ));
                    }).await;
                    if view.update(cx, |view, cx| {
                        view.apply_phase0_background_presentation(generation, cx);
                    }).is_err() {
                        break;
                    }
                }
            }).detach();
        }
        if !no_focus {
            cx.activate(true);
        }
    });
}
