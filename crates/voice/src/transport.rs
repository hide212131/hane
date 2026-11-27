//! What the voice link needs from the platform: a listener that yields
//! connections whose peer process id is known, and a way to start the engine.

use hane_link::transport::Connection;
use std::ffi::OsString;
use std::io;
use std::path::PathBuf;
use std::time::Duration;

/// A connection that can say which process is on the other end. On Windows the
/// system reports the client of a pipe instance; the engine cannot lie about it.
pub trait VoiceConnection: Connection {
    fn peer_pid(&self) -> Option<u32>;
}

impl Connection for Box<dyn VoiceConnection> {
    fn read_timeout(&mut self, buf: &mut [u8], timeout: Duration) -> io::Result<usize> {
        (**self).read_timeout(buf, timeout)
    }

    fn write_timeout(&mut self, buf: &[u8], timeout: Duration) -> io::Result<usize> {
        (**self).write_timeout(buf, timeout)
    }
}

/// Accepts connections on the dedicated channel of one launch.
pub trait VoiceListener: Send {
    /// Waits at most `timeout`.
    fn accept(&mut self, timeout: Duration) -> io::Result<Option<Box<dyn VoiceConnection>>>;
}

/// What is handed to the engine through the inherited anonymous pipe, once.
/// The token is never put on a command line, in the environment or in a file.
pub struct Handoff {
    pub pipe_name: String,
    pub token: String,
    pub parent_pid: u32,
}

impl Handoff {
    /// The JSON the engine reads from its inherited pipe: one frame.
    pub fn to_json(&self) -> Vec<u8> {
        serde_json::json!({
            "pipeName": self.pipe_name,
            "token": self.token,
            "parentPid": self.parent_pid,
        })
        .to_string()
        .into_bytes()
    }
}

/// The engine to start.
#[derive(Clone, Debug)]
pub struct EngineSpec {
    pub exe: PathBuf,
    /// Extra arguments after `--voice-engine --voice-handoff <handle>`. Used by
    /// tests to script a stand-in engine; the product passes none.
    pub extra_args: Vec<OsString>,
}

impl EngineSpec {
    pub fn new(exe: impl Into<PathBuf>) -> Self {
        Self {
            exe: exe.into(),
            extra_args: Vec::new(),
        }
    }
}

/// A running engine. Dropping it must not leave the process behind.
pub trait ChildProcess: Send {
    fn pid(&self) -> u32;
    /// The exit code once the process has ended, without waiting.
    fn exit_code(&mut self) -> Option<i32>;
    /// Ends the process and everything it started (the recognition workers).
    fn kill(&mut self);
}

/// Creates the channel and starts the engine.
pub trait Platform: Send + Sync {
    /// Creates the dedicated channel `name` (a name with a random part).
    fn listen(&self, name: &str) -> io::Result<Box<dyn VoiceListener>>;
    /// Starts the engine with `handoff` passed through an inherited anonymous
    /// pipe that only the child receives.
    fn launch(&self, spec: &EngineSpec, handoff: &Handoff) -> io::Result<Box<dyn ChildProcess>>;
}

/// The platform of this build: named pipes and a job object on Windows, a
/// private Unix socket elsewhere (development and tests only).
pub fn system_platform() -> Box<dyn Platform> {
    #[cfg(windows)]
    {
        Box::new(crate::windows::WindowsPlatform)
    }
    #[cfg(not(windows))]
    {
        Box::new(crate::unix::UnixPlatform)
    }
}
