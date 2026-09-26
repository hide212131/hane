//! App-side measurement harness, compiled only under the `instrument` feature.
//! Drives synthetic input, autoscroll, background parsing load, and idle memory
//! sampling from a single [`InstrumentationConfig`]. None of this is present in
//! the shipping binary.

use gpui::{App, AsyncApp, Focusable, WeakEntity, WindowHandle};
use hane_document::{Revision, SourceRange};
use hane_markdown::parse_document;
use hane_presentation::present_markdown;
use hane_ui::{EditorView, InstrumentationConfig};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

async fn wait_for_work_folder(
    view: &WeakEntity<EditorView>,
    cx: &mut AsyncApp,
    expected_root: Option<PathBuf>,
) -> Option<(PathBuf, usize)> {
    for _ in 0..6_000 {
        let state = view
            .update(cx, |view, _| view.measurement_work_folder_state())
            .ok()
            .flatten();
        if let Some((root, count)) = state
            && expected_root
                .as_ref()
                .is_none_or(|expected| expected == &root)
        {
            return Some((root, count));
        }
        cx.background_executor()
            .timer(Duration::from_millis(50))
            .await;
    }
    None
}

async fn wait_for_note(view: &WeakEntity<EditorView>, cx: &mut AsyncApp, path: &Path) -> bool {
    for _ in 0..1_200 {
        if view
            .update(cx, |view, _| view.measurement_note_is_loaded(path))
            .unwrap_or(false)
        {
            return true;
        }
        cx.background_executor()
            .timer(Duration::from_millis(50))
            .await;
    }
    false
}

fn record_memory(view: &WeakEntity<EditorView>, cx: &mut AsyncApp, label: &str) {
    if let Ok((rss_bytes, session_count)) =
        view.update(cx, |view, _| view.record_measurement_memory(label))
    {
        eprintln!(
            "hane_measurement_memory label={label} rss_bytes={} session_count={session_count}",
            rss_bytes.unwrap_or(0),
        );
    }
}

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
                window.focus(&view.focus_handle(cx), cx);
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
        let idle_seconds = config.measurement_idle_seconds;
        cx.spawn(async move |cx| {
            cx.background_executor()
                .timer(Duration::from_secs(idle_seconds))
                .await;
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
    if config.measure_work_folder
        || !config.work_folder_visit_counts.is_empty()
        || config.measurement_cycles > 0
    {
        let view = window
            .entity(cx)
            .expect("read Hane root entity")
            .downgrade();
        let visit_counts = config.work_folder_visit_counts.clone();
        let idle_seconds = config.measurement_idle_seconds;
        let cycle_folders = config.measurement_cycle_folders.clone();
        let cycles = config.measurement_cycles;
        let measure_work_folder = config.measure_work_folder;
        cx.spawn(async move |cx| {
            let Some((initial_root, initial_count)) =
                wait_for_work_folder(&view, cx, None).await
            else {
                eprintln!("hane_measurement_error=work_folder_scan_timeout");
                return;
            };
            let elapsed_ms = view
                .update(cx, |view, _| view.measurement_elapsed_ms())
                .unwrap_or_default();
            eprintln!(
                "hane_work_folder_ready root={} elapsed_ms={elapsed_ms:.3} indexed_markdown_files={initial_count}",
                initial_root.display(),
            );

            if !visit_counts.is_empty() {
                let mut milestones = visit_counts;
                milestones.sort_unstable();
                milestones.dedup();
                if milestones.first() == Some(&0) {
                    eprintln!("hane_measurement_error=visit_count_must_be_positive");
                    return;
                }
                let max_visits = milestones.last().copied().unwrap_or_default();
                let paths = view
                    .update(cx, |view, _| view.measurement_work_folder_note_paths())
                    .ok()
                    .flatten()
                    .unwrap_or_default();
                if max_visits > paths.len() {
                    eprintln!(
                        "hane_measurement_error=not_enough_notes requested={max_visits} available={}",
                        paths.len(),
                    );
                    return;
                }
                let mut milestone_index = 0;
                for (index, path) in paths.into_iter().take(max_visits).enumerate() {
                    if view
                        .update(cx, |view, cx| view.open_work_folder_entry(&path, cx))
                        .is_err()
                        || !wait_for_note(&view, cx, &path).await
                    {
                        eprintln!("hane_measurement_error=note_load_timeout path={}", path.display());
                        return;
                    }
                    let visited = index + 1;
                    if milestones.get(milestone_index) == Some(&visited) {
                        cx.background_executor()
                            .timer(Duration::from_secs(idle_seconds))
                            .await;
                        let label = format!("memory_after_{visited}_notes");
                        record_memory(&view, cx, &label);
                        milestone_index += 1;
                    }
                }
            } else if cycles == 0 && measure_work_folder {
                cx.background_executor()
                    .timer(Duration::from_secs(idle_seconds))
                    .await;
                record_memory(&view, cx, "memory_work_folder_idle");
            }

            if cycles > 0 {
                if cycle_folders.is_empty() {
                    eprintln!("hane_measurement_error=cycle_folders_required");
                    return;
                }
                let sample_every = cycles.div_ceil(4).max(1);
                for cycle in 1..=cycles {
                    let root = cycle_folders[cycle % cycle_folders.len()].clone();
                    let switched = view
                        .update(cx, |view, cx| view.open_external_path(&root, cx))
                        .is_ok();
                    if !switched || wait_for_work_folder(&view, cx, Some(root.clone())).await.is_none() {
                        eprintln!(
                            "hane_measurement_error=folder_switch_timeout root={}",
                            root.display(),
                        );
                        return;
                    }
                    let paths = view
                        .update(cx, |view, _| view.measurement_work_folder_note_paths())
                        .ok()
                        .flatten()
                        .unwrap_or_default();
                    if paths.len() < 2 {
                        eprintln!("hane_measurement_error=cycle_folder_needs_two_notes");
                        return;
                    }
                    for path in paths.iter().take(2) {
                        if view
                            .update(cx, |view, cx| view.open_work_folder_entry(path, cx))
                            .is_err()
                            || !wait_for_note(&view, cx, path).await
                        {
                            eprintln!(
                                "hane_measurement_error=cycle_note_load_timeout path={}",
                                path.display(),
                            );
                            return;
                        }
                    }
                    let _ = view.update(cx, |view, cx| view.run_measurement_edit_cycles(10, cx));
                    if cycle == 1 || cycle % sample_every == 0 || cycle == cycles {
                        cx.background_executor()
                            .timer(Duration::from_secs(1))
                            .await;
                        let label = format!("memory_longrun_cycle_{cycle}");
                        record_memory(&view, cx, &label);
                    }
                }
                cx.background_executor()
                    .timer(Duration::from_secs(idle_seconds))
                    .await;
                record_memory(&view, cx, "memory_longrun_idle");
                eprintln!("hane_longrun_complete cycles={cycles}");
            }
        })
        .detach();
    }
    if !config.no_focus {
        cx.activate(true);
    }
}
