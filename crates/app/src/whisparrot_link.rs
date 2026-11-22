//! App-side owner of the local link with the whisparrot voice-input app
//! (Windows). The service lives for the whole process; the settings switch
//! starts and stops it, and every request it accepts is queued here and run on
//! the GPUI thread against the registered window.

use gpui::{App, AsyncApp, Window};
use hane_link::runtime::{Environment, Runtime};
use hane_link::{HostCall, QueueHost, TickClock};
use hane_ui::{EditorView, LinkWindow, WhisparrotLinkController, run_link_call};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

/// How many requests may wait for the UI thread before new ones are refused.
const QUEUE_CAPACITY: usize = 16;

/// Hands the settings switch to the service.
struct Controller(Arc<Runtime>);

impl WhisparrotLinkController for Controller {
    fn set_enabled(&self, enabled: bool) -> Result<(), String> {
        if enabled {
            self.0.enable()
        } else {
            self.0.disable();
            Ok(())
        }
    }
}

/// The service and the queue its requests arrive on.
pub struct LinkSetup {
    runtime: Arc<Runtime>,
    calls: async_channel::Receiver<HostCall>,
    clock: Arc<dyn TickClock>,
}

impl LinkSetup {
    /// `None` when the environment cannot host the link (no profile directory):
    /// the settings switch is then not offered.
    pub fn prepare() -> Option<Self> {
        let environment = Environment::for_this_process()?;
        let clock = Arc::clone(&environment.clock);
        let (host, calls) = QueueHost::new(QUEUE_CAPACITY);
        Some(Self {
            runtime: Arc::new(Runtime::new(environment, Arc::new(host))),
            calls,
            clock,
        })
    }

    /// The hook for the editor window's settings screen.
    pub fn controller(&self) -> Arc<dyn WhisparrotLinkController> {
        Arc::new(Controller(Arc::clone(&self.runtime)))
    }
}

/// The native handle of a GPUI window.
fn hwnd_of(window: &Window) -> Option<i64> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    match window.window_handle().ok()?.as_raw() {
        RawWindowHandle::Win32(handle) => Some(handle.hwnd.get() as i64),
        _ => None,
    }
}

/// Registers `handle` as the window requests are aimed at, starts running
/// queued requests, starts the service if the user already allowed the link,
/// and removes the published descriptor when the application quits.
pub fn start(cx: &mut App, handle: gpui::WindowHandle<EditorView>, setup: LinkSetup) {
    let LinkSetup {
        runtime,
        calls,
        clock,
    } = setup;
    let registered = handle.update(cx, |view, window, _cx| {
        hwnd_of(window).map(|hwnd| (hwnd, view.whisparrot_link_enabled()))
    });
    let Ok(Some((hwnd, enabled))) = registered else {
        return;
    };
    // One window today; the registry is a list so a second one is a push.
    let windows = Rc::new(RefCell::new(vec![LinkWindow {
        hwnd,
        window_id: 1,
        handle,
    }]));
    if enabled && let Err(message) = runtime.enable() {
        eprintln!("whisparrot link did not start: {message}");
    }
    cx.spawn({
        let windows = Rc::clone(&windows);
        async move |cx: &mut AsyncApp| {
            while let Ok(call) = calls.recv().await {
                let windows = Rc::clone(&windows);
                let clock = Arc::clone(&clock);
                if cx
                    .update(move |app| run_link_call(&windows.borrow(), &*clock, call, app))
                    .is_err()
                {
                    // The application is shutting down.
                    break;
                }
            }
        }
    })
    .detach();
    let quitting = Arc::clone(&runtime);
    cx.on_app_quit(move |_| {
        // A normal exit leaves no descriptor behind.
        quitting.disable();
        async {}
    })
    .detach();
}
