//! The engine as the editor uses it: find it, start it on demand, send the
//! recording controls, and receive the validated events.

use crate::discovery::{self, Unavailable};
use crate::link::{Control, EngineLink, INBOUND_CAPACITY, Inbound, Sink, Timeouts};
use crate::transport::{Platform, system_platform};
use crate::wire::Context;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// What the editor calls to record. Replaceable: the UI holds this, so tests
/// and the development script need no engine.
pub trait VoiceControl: Send + Sync {
    /// Starts recording for `session_id`, launching the engine first when it is
    /// not running. `Err` is a reason fit for the status line; nothing was
    /// started.
    fn begin(&self, session_id: &str, context: Option<Context>) -> Result<(), String>;
    /// Ends recording and asks for the remaining results. Repeating it has no
    /// further effect.
    fn stop(&self, session_id: &str);
    /// Ends recording at once. Repeating it has no further effect.
    fn cancel(&self, session_id: &str);
    /// Closes the connection after a contract violation and ends the engine,
    /// without waiting for it. The next recording starts a new launch.
    fn disconnect(&self);
    /// Ends the engine and everything it started, and waits until it is gone.
    /// Called when Hane exits.
    fn shutdown(&self);
}

/// Where to look for the executable.
#[derive(Clone, Debug, Default)]
pub struct EngineLocation {
    /// The registration file whisparrot writes.
    pub registration: Option<PathBuf>,
    /// An explicitly chosen executable (development and verification).
    pub explicit: Option<PathBuf>,
    /// Extra arguments for the engine, for tests that script a stand-in. The
    /// product passes none.
    pub extra_args: Vec<std::ffi::OsString>,
}

pub struct VoiceEngine {
    platform: Arc<dyn Platform>,
    timeouts: Timeouts,
    location: Mutex<EngineLocation>,
    inbound: async_channel::Sender<Inbound>,
    /// The launch whose messages are still delivered.
    current: Arc<AtomicU64>,
    launches: AtomicU64,
    link: Mutex<Option<EngineLink>>,
}

impl VoiceEngine {
    /// The engine of this platform. Events and the loss notice arrive on the
    /// returned receiver, in order.
    pub fn new(location: EngineLocation) -> (Self, async_channel::Receiver<Inbound>) {
        Self::with_platform(Arc::from(system_platform()), location, Timeouts::default())
    }

    pub fn with_platform(
        platform: Arc<dyn Platform>,
        location: EngineLocation,
        timeouts: Timeouts,
    ) -> (Self, async_channel::Receiver<Inbound>) {
        let (inbound, receiver) = async_channel::bounded(INBOUND_CAPACITY);
        (
            Self {
                platform,
                timeouts,
                location: Mutex::new(location),
                inbound,
                current: Arc::new(AtomicU64::new(0)),
                launches: AtomicU64::new(0),
                link: Mutex::new(None),
            },
            receiver,
        )
    }

    /// Changes the explicit executable (a setting) for the next launch.
    pub fn set_explicit(&self, explicit: Option<PathBuf>) {
        self.location.lock().expect("location").explicit = explicit;
    }

    /// The registration check and explicit setting, as the status line needs
    /// them before anything is started.
    pub fn check(&self) -> Result<(), Unavailable> {
        let location = self.location.lock().expect("location").clone();
        discovery::discover(location.registration.as_deref(), location.explicit.as_deref())
            .map(|_| ())
    }

    fn running(&self) -> Option<std::sync::MutexGuard<'_, Option<EngineLink>>> {
        let mut link = self.link.lock().expect("link");
        if link.as_ref().is_some_and(|link| !link.is_alive()) {
            *link = None;
        }
        Some(link)
    }
}

impl VoiceControl for VoiceEngine {
    fn begin(&self, session_id: &str, context: Option<Context>) -> Result<(), String> {
        let location = self.location.lock().expect("location").clone();
        let mut link = self.running().expect("guard");
        if link.is_none() {
            let mut spec = discovery::discover(
                location.registration.as_deref(),
                location.explicit.as_deref(),
            )
            .map_err(|reason| reason.message())?;
            spec.extra_args = location.extra_args.clone();
            let launch = self.launches.fetch_add(1, Ordering::AcqRel) + 1;
            // Messages of an earlier launch stop being delivered from here on.
            self.current.store(launch, Ordering::Release);
            let sink = Sink {
                tx: self.inbound.clone(),
                launch,
                current: Arc::clone(&self.current),
            };
            let started = EngineLink::start(Arc::clone(&self.platform), spec, self.timeouts, sink)
                .map_err(|_| "音声エンジンを起動できませんでした".to_owned())?;
            *link = Some(started);
        }
        let sent = link.as_ref().is_some_and(|link| {
            link.send(Control::Begin {
                session_id: session_id.to_owned(),
                context,
            })
        });
        if sent {
            Ok(())
        } else {
            Err("音声エンジンへ録音開始を送れませんでした".to_owned())
        }
    }

    fn stop(&self, session_id: &str) {
        if let Some(link) = self.running().expect("guard").as_ref() {
            link.send(Control::Stop(session_id.to_owned()));
        }
    }

    fn cancel(&self, session_id: &str) {
        if let Some(link) = self.running().expect("guard").as_ref() {
            link.send(Control::Cancel(session_id.to_owned()));
        }
    }

    fn disconnect(&self) {
        let link = self.link.lock().expect("link").take();
        if let Some(link) = link {
            self.current.store(0, Ordering::Release);
            // Ending the launch can wait on the engine; not on the UI thread.
            let _ = std::thread::Builder::new()
                .name("hane-voice-disconnect".to_owned())
                .spawn(move || link.shutdown());
        }
    }

    fn shutdown(&self) {
        let link = self.link.lock().expect("link").take();
        if let Some(link) = link {
            self.current.store(0, Ordering::Release);
            link.shutdown();
        }
    }
}

impl Drop for VoiceEngine {
    fn drop(&mut self) {
        self.shutdown();
    }
}
