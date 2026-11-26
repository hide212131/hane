//! App-side owner of the connection to the whisparrot voice engine.
//!
//! The engine is the installed whisparrot, found through its registration (or
//! the `voice_engine_path` setting) and started as a child only when the user
//! records. Its events arrive on a bounded queue and are applied on the GPUI
//! thread; the child (and the recognition workers) are ended when Hane quits.

use gpui::{App, AsyncApp, WindowHandle};
use hane_ui::EditorView;
use hane_voice::{EngineLocation, VoiceControl, VoiceEngine, registration_path};
use std::sync::Arc;

/// Gives the window its engine, starts delivering the engine's messages to it
/// and ends the engine when the application quits.
pub fn start(cx: &mut App, window: WindowHandle<EditorView>) {
    let explicit = window
        .update(cx, |view, _window, _cx| view.voice_engine_path())
        .ok()
        .flatten();
    let (engine, inbound) = VoiceEngine::new(EngineLocation {
        registration: registration_path(),
        explicit,
        extra_args: Vec::new(),
    });
    let engine = Arc::new(engine);
    let control: Arc<dyn VoiceControl> = engine.clone();
    let _ = window.update(cx, |view, _window, _cx| {
        view.attach_voice_control(Some(control));
    });
    cx.spawn(async move |cx: &mut AsyncApp| {
        while let Ok(message) = inbound.recv().await {
            let _ = window.update(cx, |view, _window, cx| view.voice_engine_inbound(message, cx));
        }
    })
    .detach();
    cx.on_app_quit(move |_| {
        // A normal exit leaves no engine behind; on an abnormal one the job
        // object (Windows) or the closed connection does the same.
        engine.shutdown();
        async {}
    })
    .detach();
}
