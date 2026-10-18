//! The limited-health start of an update's new version (Issue #510, U4).
//!
//! The helper may commit an update only after the new version has shown that
//! its GUI can really start. This process therefore opens a minimal "updating"
//! window that shows no document or setting, and reports `Healthy` only after
//! that window has drawn its first frame. Until the helper's durable `Commit`
//! arrives nothing else starts: no settings store, document restore, AI
//! service, autosave, migration or external open. A window that cannot be
//! created or drawn never reports `Healthy`, so the helper times out (or sees
//! the pipe end) and rolls back.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, TryRecvError};
use std::time::Duration;

use gpui::{
    App, AppContext, Bounds, Context, Global, IntoElement, Render, Window, WindowBounds,
    WindowOptions, div, prelude::*, px, rgb, size,
};
use hane_ui::AppAssets;
use hane_update::health::VerifiedChild;
use hane_update::startup::RuntimeGuard;

use crate::update_gate::LIMITED_COMMIT_WAIT;

const EXIT_NOT_COMMITTED: i32 = 70;
/// How long the process may take to draw its first frame before it gives up
/// without reporting anything (the helper's own health timeout is longer).
const FIRST_FRAME_TIMEOUT: Duration = Duration::from_secs(30);

/// Holds the health report back until the first frame has been drawn.
pub struct HealthGate<F> {
    report: Option<F>,
}

impl<F> HealthGate<F> {
    pub fn new(report: F) -> Self {
        Self { report: Some(report) }
    }

    /// The first drawn frame releases the report, exactly once.
    pub fn first_frame_drawn(&mut self) -> Option<F> {
        self.report.take()
    }
}

/// Runs `normal` only when the helper's commit was confirmed.
pub fn start_after_commit<G, E>(
    outcome: Result<G, E>,
    normal: impl FnOnce(G),
) -> Result<(), E> {
    outcome.map(normal)
}

/// Keeps the shared runtime lease for the rest of the process.
struct HeldRuntimeLease(#[allow(dead_code, reason = "held for the process lifetime")] RuntimeGuard);

impl Global for HeldRuntimeLease {}

struct LimitedView {
    on_first_frame: Option<Box<dyn FnOnce()>>,
}

impl Render for LimitedView {
    fn render(&mut self, window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(signal) = self.on_first_frame.take() {
            // Callbacks run when the next frame is requested, i.e. after the
            // frame that rendered this view has been drawn and presented.
            window.on_next_frame(move |_, _| signal());
            window.request_animation_frame();
        }
        div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .bg(rgb(0xf6f6f6))
            .text_color(rgb(0x333333))
            .child("Hane を更新しています…")
    }
}

pub fn run(verified: Box<VerifiedChild>) {
    let state_root = verified.state_root().to_path_buf();
    let (done_tx, done_rx) = mpsc::channel::<Result<RuntimeGuard, String>>();
    let drawn = Arc::new(AtomicBool::new(false));
    {
        let drawn = Arc::clone(&drawn);
        std::thread::spawn(move || {
            std::thread::sleep(FIRST_FRAME_TIMEOUT);
            if !drawn.load(Ordering::SeqCst) {
                eprintln!("Hane could not draw its update window in time.");
                std::process::exit(EXIT_NOT_COMMITTED);
            }
        });
    }
    let mut gate = HealthGate::new(move || {
        verified
            .report_healthy(std::io::stdin(), std::io::stdout(), LIMITED_COMMIT_WAIT)
            .map_err(|err| err.to_string())
    });
    let on_first_frame: Box<dyn FnOnce()> = Box::new(move || {
        drawn.store(true, Ordering::SeqCst);
        if let Some(report) = gate.first_frame_drawn() {
            // Waiting for `Commit` must not block drawing.
            std::thread::spawn(move || {
                let _ = done_tx.send(report());
            });
        }
    });

    // Finder callbacks are registered now but only queue paths; the receiver
    // reaches the normal editor solely through `prepare_launch` after the
    // durable commit, so nothing is opened before then.
    let (finder_sender, finder_requests) = crate::finder_channel();
    crate::application_with_finder_open(finder_sender)
        .with_assets(AppAssets)
        .run(move |cx: &mut App| {
            let bounds = Bounds::centered(None, size(px(420.), px(160.)), cx);
            let opened = cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    ..Default::default()
                },
                |_, cx| {
                    cx.new(|_| LimitedView {
                        on_first_frame: Some(on_first_frame),
                    })
                },
            );
            let Ok(window) = opened else {
                // No `Healthy` is ever sent; the helper rolls back.
                eprintln!("Hane could not open its update window.");
                std::process::exit(EXIT_NOT_COMMITTED);
            };
            cx.activate(true);
            cx.spawn(async move |cx| {
                loop {
                    cx.background_executor()
                        .timer(Duration::from_millis(40))
                        .await;
                    let result = match done_rx.try_recv() {
                        Ok(result) => result,
                        Err(TryRecvError::Empty) => continue,
                        Err(TryRecvError::Disconnected) => {
                            std::process::exit(EXIT_NOT_COMMITTED);
                        }
                    };
                    let state_root = state_root.clone();
                    cx.update(|cx| {
                        let started = start_after_commit(result, |guard| {
                            cx.set_global(HeldRuntimeLease(guard));
                            // Only now, with the commit confirmed, is the
                            // verified state root pinned and normal startup run.
                            if !hane_session::pin_state_root(state_root) {
                                std::process::exit(EXIT_NOT_COMMITTED);
                            }
                            match crate::prepare_launch(None, finder_requests) {
                                Some(launch) => {
                                    crate::open_editor(cx, launch);
                                    let _ = window.update(cx, |_, window, _| window.remove_window());
                                }
                                None => cx.quit(),
                            }
                        });
                        if let Err(why) = started {
                            eprintln!("Hane could not confirm the update it was started for ({why}).");
                            std::process::exit(EXIT_NOT_COMMITTED);
                        }
                    });
                    return;
                }
            })
            .detach();
        });
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;

    #[test]
    fn the_report_is_not_released_before_the_first_frame() {
        let ran = Cell::new(0);
        let mut gate = HealthGate::new(|| ran.set(ran.get() + 1));
        assert_eq!(ran.get(), 0, "constructing the gate sends nothing");
        let report = gate.first_frame_drawn().expect("released by the first frame");
        assert_eq!(ran.get(), 0, "releasing is not running");
        report();
        assert_eq!(ran.get(), 1);
        assert!(gate.first_frame_drawn().is_none(), "reported only once");
    }

    #[test]
    fn a_window_that_failed_never_reports_healthy() {
        // A window that never drew leaves the gate at the end of the scope
        // without releasing the report.
        let _gate = HealthGate::new(|| panic!("Healthy must not be sent"));
    }

    #[test]
    fn normal_services_start_only_after_a_confirmed_commit() {
        let started = Cell::new(false);
        let committed: Result<u8, String> = Ok(1);
        assert!(start_after_commit(committed, |_| started.set(true)).is_ok());
        assert!(started.get());

        for failure in ["no commit", "pipe closed", "expired", "helper crashed"] {
            let started = Cell::new(false);
            let outcome: Result<u8, String> = Err(failure.to_owned());
            assert!(start_after_commit(outcome, |_| started.set(true)).is_err());
            assert!(!started.get(), "{failure}: normal startup must not run");
        }
    }
}
