//! App-side measurement harness, compiled only under the `instrument` feature.
//! Drives synthetic input, autoscroll, background parsing load, and idle memory
//! sampling from a single [`InstrumentationConfig`]. None of this is present in
//! the shipping binary.

use gpui::{App, Focusable, Timer, WindowHandle};
use hane_document::{Revision, SourceRange};
use hane_markdown::parse_document;
use hane_presentation::present_markdown;
use hane_ui::{EditorView, InstrumentationConfig};
use std::sync::Arc;
use std::time::Duration;

pub(crate) fn apply(
    window: &WindowHandle<EditorView>,
    config: &InstrumentationConfig,
    cx: &mut App,
) {
    if let Some(offset) = config.measurement_cursor_offset {
        window
            .update(cx, |view, _, cx| {
                view.set_cursor_offset_for_measurement(offset, cx)
            })
            .expect("set development cursor")
            .expect("HANE_MEASUREMENT_CURSOR_OFFSET must be a valid character boundary");
    }
    if let Some(count) = config.dev_cursor_down {
        window
            .update(cx, |view, _, cx| {
                view.move_cursor_down_for_development(count, cx)
            })
            .expect("move development cursor down")
            .expect("development cursor movement must succeed");
    }
    if !config.no_focus {
        window
            .update(cx, |view, window, cx| {
                window.focus(&view.focus_handle(cx));
            })
            .expect("focus editor");
    }
    if config.autoscroll {
        window
            .update(cx, |view, _, cx| {
                view.enable_display_linked_scroll_measurement();
                cx.notify();
            })
            .expect("schedule display-linked scroll measurement");
    }
    if config.measure_idle_rss {
        let view = window
            .entity(cx)
            .expect("read Hane root entity")
            .downgrade();
        cx.spawn(async move |cx| {
            Timer::after(Duration::from_secs(30)).await;
            let rss = hane_metrics::process_memory_bytes();
            let _ = view.update(cx, |view, _| view.record_phase0_idle_memory(rss));
        })
        .detach();
    }
    if config.background_presentation {
        let view = window
            .entity(cx)
            .expect("read Hane root entity")
            .downgrade();
        cx.spawn(async move |cx| {
            let source: Arc<str> = ("background **presentation update** 日本語 🙂\n")
                .repeat(16_384)
                .into();
            let range = SourceRange::new(0, source.len());
            for generation in 1_u64.. {
                let source = Arc::clone(&source);
                cx.background_executor()
                    .spawn(async move {
                        let revision = Revision(generation);
                        std::hint::black_box(parse_document(revision, range, &source));
                        std::hint::black_box(present_markdown(
                            generation, revision, range, &source, 26.0,
                        ));
                    })
                    .await;
                if view
                    .update(cx, |view, cx| {
                        view.apply_phase0_background_presentation(generation, cx);
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
    }
    if !config.no_focus {
        cx.activate(true);
    }
}
