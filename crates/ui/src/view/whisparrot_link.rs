//! The editor side of the local link with the whisparrot voice-input app.
//!
//! The link service (`hane-link`) authenticates a connection and hands each
//! request to the UI thread, which executes it here against the window's
//! documents. Everything that reads or changes a document runs in one GPUI turn
//! of [`EditorView::link_execute`]: the target is verified and the edit applied
//! without yielding, so a check can never go stale before the edit it guards.

use super::*;
use hane_link::message::{ErrorCode, HostRequest, LinkError, LinkResult};

/// The application's hook for starting and stopping the link service when the
/// user changes the setting. Implemented by the app, which owns the service.
pub trait WhisparrotLinkController: Send + Sync {
    /// Starts (`true`) or stops (`false`) the service. An error means the
    /// service did not reach the requested state; its message must not contain
    /// secrets.
    fn set_enabled(&self, enabled: bool) -> Result<(), String>;
}

/// Link state that belongs to one window.
#[derive(Default)]
pub(super) struct LinkViewState {
    pub(super) controller: Option<Arc<dyn WhisparrotLinkController>>,
}

/// A window the link may address, by the native handle the client captured.
pub struct LinkWindow {
    /// The native window handle (`HWND` on Windows).
    pub hwnd: i64,
    /// This window's id within the service instance, part of every document id.
    pub window_id: u64,
    pub handle: gpui::WindowHandle<EditorView>,
}

/// Runs one queued request on the UI thread and answers it. A request for a
/// window that is not registered (or whose window is gone) is a stale target.
/// The ticket is claimed in the same turn that executes the request, so a
/// deadline that passed while the request waited stops it before it starts.
pub fn run_link_call(
    windows: &[LinkWindow],
    clock: &dyn hane_link::TickClock,
    call: hane_link::HostCall,
    cx: &mut gpui::App,
) {
    let hwnd = match &call.request {
        HostRequest::ContextGet(params) => params.hwnd.0,
        HostRequest::Insert(params) => params.hwnd.0,
        HostRequest::Replace(params) => params.hwnd.0,
    };
    let result = match windows.iter().find(|window| window.hwnd == hwnd) {
        None => Err(LinkError::not_applied(ErrorCode::StaleTarget)),
        Some(window) => {
            let outcome = window.handle.update(cx, |view, window_ref, cx| {
                call.ticket
                    .begin(clock.now_ms(), call.request.deadline_tick())?;
                let context = LinkCall {
                    instance_id: &call.instance_id,
                    window_id: window.window_id,
                    now: Instant::now(),
                };
                view.link_execute(&call.request, &context, window_ref, cx)
            });
            outcome.unwrap_or(Err(LinkError::not_applied(ErrorCode::StaleTarget)))
        }
    };
    let _ = call.reply.send(result);
}

/// What the app tells the view about a call it dispatches.
pub struct LinkCall<'a> {
    /// The service instance the request came in on.
    pub instance_id: &'a str,
    /// This window's registration id within the service instance.
    pub window_id: u64,
    pub now: Instant,
}

impl EditorView {
    /// Installs the hook that starts and stops the link service. Without one
    /// (platforms where the link does not exist) the setting is not offered.
    pub fn attach_whisparrot_link_controller(
        &mut self,
        controller: Option<Arc<dyn WhisparrotLinkController>>,
    ) {
        self.link.controller = controller;
    }

    /// Whether the user allowed the link. Off until they turn it on.
    pub fn whisparrot_link_enabled(&self) -> bool {
        self.settings.whisparrot_link
    }

    /// Whether the settings screen offers the switch at all.
    pub(crate) fn whisparrot_link_available(&self) -> bool {
        self.link.controller.is_some()
    }

    /// Turns the link on or off: starts or stops the service first, and keeps
    /// the stored setting in step with what actually happened. Returns whether
    /// the setting now has the requested value.
    pub(crate) fn set_whisparrot_link(&mut self, enabled: bool, cx: &mut Context<Self>) -> bool {
        if self.settings.whisparrot_link == enabled {
            return true;
        }
        if let Some(controller) = self.link.controller.clone()
            && let Err(message) = controller.set_enabled(enabled)
        {
            // The service did not change state, so neither does the setting.
            self.status = Some(format!(
                "whisparrot との連携を切り替えられませんでした: {message}"
            ));
            cx.notify();
            return false;
        }
        self.settings.whisparrot_link = enabled;
        // A failed write keeps the change for this run, like the other settings.
        self.store_settings();
        cx.notify();
        true
    }

    /// Carries out one request from the link on the UI thread.
    pub fn link_execute(
        &mut self,
        request: &HostRequest,
        call: &LinkCall<'_>,
        _window: &Window,
        _cx: &mut Context<Self>,
    ) -> LinkResult {
        let _ = (request, call);
        Err(LinkError::not_applied(ErrorCode::InvalidRequest))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Records what the view asked the service to do, and can refuse.
    #[derive(Default)]
    struct FakeController {
        calls: Mutex<Vec<bool>>,
        fail: Mutex<bool>,
    }

    impl WhisparrotLinkController for FakeController {
        fn set_enabled(&self, enabled: bool) -> Result<(), String> {
            self.calls.lock().unwrap().push(enabled);
            if *self.fail.lock().unwrap() {
                Err("could not start".to_owned())
            } else {
                Ok(())
            }
        }
    }

    fn open(
        cx: &mut gpui::TestAppContext,
        stores: StateStores,
        controller: Option<Arc<dyn WhisparrotLinkController>>,
    ) -> (gpui::Entity<EditorView>, &mut gpui::VisualTestContext) {
        cx.update(crate::actions::register_key_bindings);
        let (view, cx) = cx.add_window_view(|_, cx| {
            let mut view = EditorView::from_sessions(
                SessionSet::with_untitled("本文\n", "Untitled"),
                Arc::new(OsFileService),
                stores,
                cx,
            );
            view.attach_whisparrot_link_controller(controller);
            view
        });
        cx.simulate_resize(gpui::size(px(960.0), px(760.0)));
        cx.run_until_parked();
        (view, cx)
    }

    #[gpui::test]
    fn the_link_is_off_by_default_and_the_switch_starts_and_stops_the_service(
        cx: &mut gpui::TestAppContext,
    ) {
        let stores = StateStores::memory();
        let controller = Arc::new(FakeController::default());
        let (view, cx) = open(cx, stores.clone(), Some(controller.clone()));
        view.read_with(cx, |view, _| assert!(!view.whisparrot_link_enabled()));
        assert!(controller.calls.lock().unwrap().is_empty());

        assert!(view.update(cx, |view, cx| view.set_whisparrot_link(true, cx)));
        assert!(stores.settings().load().whisparrot_link, "persisted");
        // Switching to the value it already has does not touch the service.
        assert!(view.update(cx, |view, cx| view.set_whisparrot_link(true, cx)));
        assert!(view.update(cx, |view, cx| view.set_whisparrot_link(false, cx)));
        assert!(!stores.settings().load().whisparrot_link);
        assert_eq!(*controller.calls.lock().unwrap(), vec![true, false]);
    }

    #[gpui::test]
    fn a_service_that_cannot_start_leaves_the_setting_off(cx: &mut gpui::TestAppContext) {
        let stores = StateStores::memory();
        let controller = Arc::new(FakeController::default());
        *controller.fail.lock().unwrap() = true;
        let (view, cx) = open(cx, stores.clone(), Some(controller));
        assert!(!view.update(cx, |view, cx| view.set_whisparrot_link(true, cx)));
        view.read_with(cx, |view, _| {
            assert!(!view.whisparrot_link_enabled());
            assert!(
                view.status
                    .as_deref()
                    .is_some_and(|status| status.contains("連携"))
            );
        });
        assert!(!stores.settings().load().whisparrot_link);
    }

    #[gpui::test]
    fn the_settings_screen_offers_the_switch_only_where_the_link_exists(
        cx: &mut gpui::TestAppContext,
    ) {
        for (controller, expected) in [
            (
                Some(Arc::new(FakeController::default()) as Arc<dyn WhisparrotLinkController>),
                true,
            ),
            (None, false),
        ] {
            let (view, cx) = open(cx, StateStores::memory(), controller);
            cx.update(|window, app| view.update(app, |view, cx| view.open_settings(window, cx)));
            cx.run_until_parked();
            assert_eq!(
                cx.debug_bounds("settings-whisparrot-link-card").is_some(),
                expected
            );
        }
    }

    #[gpui::test]
    fn clicking_the_checkbox_flips_the_setting(cx: &mut gpui::TestAppContext) {
        let stores = StateStores::memory();
        let controller = Arc::new(FakeController::default());
        let (view, cx) = open(cx, stores.clone(), Some(controller.clone()));
        cx.update(|window, app| view.update(app, |view, cx| view.open_settings(window, cx)));
        cx.run_until_parked();
        let card = cx
            .debug_bounds("settings-whisparrot-link-card")
            .expect("the link card is laid out");
        cx.simulate_click(
            gpui::point(card.left() + px(25.0), card.top() + px(24.0)),
            gpui::Modifiers::none(),
        );
        cx.run_until_parked();
        view.read_with(cx, |view, _| assert!(view.whisparrot_link_enabled()));
        assert!(stores.settings().load().whisparrot_link);
        assert_eq!(*controller.calls.lock().unwrap(), vec![true]);
    }
}
