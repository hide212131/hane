//! Starting and stopping the link as the user turns it on and off.

use crate::auth::Credentials;
use crate::descriptor::{self, Descriptor, ProcessProbe};
use crate::host::{Host, TickClock};
use crate::server::Shared;
use crate::service::RunningService;
use crate::transport::BoxListener;
use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// Creates the listener for a pipe name.
pub type ListenFn = Box<dyn Fn(&str) -> io::Result<BoxListener> + Send + Sync>;

/// Everything the runtime needs from the platform, so tests can substitute
/// an in-memory listener and a scratch directory.
pub struct Environment {
    /// Where the descriptor directory lives (`%LOCALAPPDATA%` on Windows) and
    /// the directories created beneath it.
    pub descriptor_base: PathBuf,
    pub descriptor_dirs: Vec<String>,
    pub pid: u32,
    /// The creation time of this process, as the descriptor records it.
    pub process_created: u64,
    pub clock: Arc<dyn TickClock>,
    pub probe: Arc<dyn ProcessProbe + Send + Sync>,
    /// Creates the listener for a pipe name.
    pub listen: ListenFn,
}

#[cfg(windows)]
impl Environment {
    /// The real environment of this process: `%LOCALAPPDATA%\Hane\integration\
    /// whisparrot`, named pipes, the system tick clock. `None` when the profile
    /// directory or this process's creation time cannot be determined.
    pub fn for_this_process() -> Option<Self> {
        use crate::platform::{SystemProcesses, SystemTick, current_process_creation_time};
        use crate::windows::PipeListener;
        let base = PathBuf::from(std::env::var_os("LOCALAPPDATA")?);
        Some(Self {
            descriptor_base: base,
            descriptor_dirs: vec!["Hane".into(), "integration".into(), "whisparrot".into()],
            pid: std::process::id(),
            process_created: current_process_creation_time()?,
            clock: Arc::new(SystemTick),
            probe: Arc::new(SystemProcesses),
            listen: Box::new(|name| PipeListener::create(name).map(BoxListener::new)),
        })
    }
}

struct Running {
    service: RunningService,
    descriptor_path: PathBuf,
}

/// The link service of this process: off until [`Runtime::enable`].
pub struct Runtime {
    environment: Environment,
    host: Arc<dyn Host>,
    running: Mutex<Option<Running>>,
}

impl Runtime {
    pub fn new(environment: Environment, host: Arc<dyn Host>) -> Self {
        Self {
            environment,
            host,
            running: Mutex::new(None),
        }
    }

    pub fn is_running(&self) -> bool {
        self.running.lock().expect("runtime lock").is_some()
    }

    /// The instance id of the running service.
    pub fn instance_id(&self) -> Option<String> {
        self.running
            .lock()
            .expect("runtime lock")
            .as_ref()
            .map(|running| {
                running
                    .service
                    .shared()
                    .credentials
                    .instance_id()
                    .to_owned()
            })
    }

    /// Starts the service with fresh credentials and publishes its descriptor.
    /// Enabling an already running service restarts it, which revokes the token
    /// handed out before. The error text never contains secrets.
    pub fn enable(&self) -> Result<(), String> {
        let mut running = self.running.lock().expect("runtime lock");
        if let Some(previous) = running.take() {
            stop(previous);
        }
        let credentials =
            Credentials::generate().ok_or_else(|| "乱数を生成できません".to_owned())?;
        let env = &self.environment;
        let dir = descriptor::ensure_private_dir(
            &env.descriptor_base,
            &env.descriptor_dirs
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
        )
        .map_err(|_| "連携用のフォルダーを準備できません".to_owned())?;
        descriptor::sweep_stale(&dir, env.pid, env.probe.as_ref());
        let pipe_name = descriptor::pipe_name(env.pid, credentials.instance_id());
        let listener =
            (env.listen)(&pipe_name).map_err(|_| "連携用の接続口を作成できません".to_owned())?;
        let published = Descriptor::new(env.pid, env.process_created, &credentials);
        // The descriptor appears only once the pipe exists to be connected to.
        let service = RunningService::start(
            listener,
            Shared::new(credentials, Arc::clone(&self.host), Arc::clone(&env.clock)),
        );
        match descriptor::publish(&dir, &published) {
            Ok(descriptor_path) => {
                *running = Some(Running {
                    service,
                    descriptor_path,
                });
                Ok(())
            }
            Err(_) => {
                service.stop();
                Err("連携用の情報を書き込めません".to_owned())
            }
        }
    }

    /// Stops the service: connections end, the descriptor is removed and the
    /// token is gone for good.
    pub fn disable(&self) {
        if let Some(previous) = self.running.lock().expect("runtime lock").take() {
            stop(previous);
        }
    }
}

fn stop(running: Running) {
    // Remove the descriptor first so no new client can pick it up.
    descriptor::remove(&running.descriptor_path);
    running.service.stop();
}

impl Drop for Runtime {
    fn drop(&mut self) {
        self.disable();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::QueueHost;
    use crate::testing::{ManualClock, MemoryConnection, MemoryConnector, listener};
    use serde_json::{Value, json};
    use std::time::Duration;

    struct Alive;

    impl ProcessProbe for Alive {
        fn creation_time(&self, _pid: u32) -> Option<u64> {
            Some(7)
        }
    }

    /// Removes its directory when dropped.
    struct Scratch(PathBuf);

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    struct Fixture {
        runtime: Runtime,
        scratch: Scratch,
        connector: Arc<Mutex<Option<MemoryConnector>>>,
    }

    fn fixture(name: &str) -> Fixture {
        let base =
            std::env::temp_dir().join(format!("hane-link-runtime-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let connector = Arc::new(Mutex::new(None));
        let slot = Arc::clone(&connector);
        let (host, _receiver) = QueueHost::new(4);
        let environment = Environment {
            descriptor_base: base.clone(),
            descriptor_dirs: vec!["Hane".into(), "integration".into(), "whisparrot".into()],
            pid: std::process::id(),
            process_created: 7,
            clock: ManualClock::new(1),
            probe: Arc::new(Alive),
            listen: Box::new(move |_name| {
                let (listener, connector) = listener();
                *slot.lock().unwrap() = Some(connector);
                Ok(BoxListener::new(listener))
            }),
        };
        Fixture {
            runtime: Runtime::new(environment, Arc::new(host)),
            scratch: Scratch(base),
            connector,
        }
    }

    impl Fixture {
        fn descriptor(&self) -> Option<Descriptor> {
            let path = self
                .scratch
                .0
                .join("Hane/integration/whisparrot")
                .join(Descriptor::file_name(std::process::id()));
            serde_json::from_slice(&std::fs::read(path).ok()?).ok()
        }

        fn connect(&self) -> MemoryConnection {
            self.connector.lock().unwrap().as_ref().unwrap().connect()
        }
    }

    fn hello(conn: &mut MemoryConnection, token: &str) -> Option<Value> {
        let body = json!({"id": 1, "method": "hello", "params": {
            "token": token,
            "minVersion": {"major": 1, "minor": 0},
            "maxVersion": {"major": 1, "minor": 0},
        }});
        conn.send_frame(&serde_json::to_vec(&body).unwrap())
            .unwrap();
        conn.receive_frame(Duration::from_secs(3))
            .unwrap()
            .map(|bytes| serde_json::from_slice(&bytes).unwrap())
    }

    #[test]
    fn nothing_is_published_until_enabled() {
        let fixture = fixture("off");
        assert!(!fixture.runtime.is_running());
        assert!(fixture.descriptor().is_none());
        assert!(!fixture.scratch.0.join("Hane").exists());
    }

    #[test]
    fn enabling_publishes_a_descriptor_whose_token_authenticates() {
        let fixture = fixture("enable");
        fixture.runtime.enable().unwrap();
        let descriptor = fixture.descriptor().expect("descriptor published");
        assert_eq!(descriptor.pid, std::process::id());
        assert_eq!(
            Some(descriptor.instance_id.clone()),
            fixture.runtime.instance_id()
        );
        assert_eq!(descriptor.token.len(), 64);
        assert!(
            descriptor
                .pipe_name
                .starts_with(&format!("Hane.Whisparrot.{}.", descriptor.pid))
        );
        let mut conn = fixture.connect();
        let reply = hello(&mut conn, &descriptor.token).expect("a reply");
        assert_eq!(reply["result"]["instanceId"], json!(descriptor.instance_id));
    }

    #[test]
    fn disabling_removes_the_descriptor_and_closes_connections() {
        let fixture = fixture("disable");
        fixture.runtime.enable().unwrap();
        let token = fixture.descriptor().unwrap().token;
        let mut conn = fixture.connect();
        assert!(hello(&mut conn, &token).unwrap().get("result").is_some());
        fixture.runtime.disable();
        assert!(!fixture.runtime.is_running());
        assert!(fixture.descriptor().is_none(), "descriptor removed");
        assert!(
            conn.receive_frame(Duration::from_secs(3))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn re_enabling_issues_a_new_token_and_the_old_one_is_refused() {
        let fixture = fixture("reenable");
        fixture.runtime.enable().unwrap();
        let first = fixture.descriptor().unwrap();
        fixture.runtime.disable();
        fixture.runtime.enable().unwrap();
        let second = fixture.descriptor().unwrap();
        assert_ne!(first.token, second.token);
        assert_ne!(first.instance_id, second.instance_id);
        let mut conn = fixture.connect();
        let reply = hello(&mut conn, &first.token).unwrap();
        assert_eq!(reply["error"]["code"], json!("UNAUTHORIZED"));
        let mut conn = fixture.connect();
        assert!(
            hello(&mut conn, &second.token)
                .unwrap()
                .get("result")
                .is_some()
        );
    }

    #[test]
    fn enabling_twice_restarts_with_a_fresh_token() {
        let fixture = fixture("twice");
        fixture.runtime.enable().unwrap();
        let first = fixture.descriptor().unwrap().token;
        fixture.runtime.enable().unwrap();
        assert_ne!(first, fixture.descriptor().unwrap().token);
    }

    #[test]
    fn a_failing_listener_leaves_nothing_published() {
        let base =
            std::env::temp_dir().join(format!("hane-link-runtime-fail-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let (host, _receiver) = QueueHost::new(1);
        let runtime = Runtime::new(
            Environment {
                descriptor_base: base.clone(),
                descriptor_dirs: vec!["w".into()],
                pid: 1,
                process_created: 1,
                clock: ManualClock::new(1),
                probe: Arc::new(Alive),
                listen: Box::new(|_| Err(io::Error::from(io::ErrorKind::AddrInUse))),
            },
            Arc::new(host),
        );
        let error = runtime.enable().unwrap_err();
        assert!(!runtime.is_running());
        assert!(!base.join("w/1.json").exists());
        assert!(!error.contains(&"0".repeat(8)), "no secrets in the message");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn dropping_the_runtime_removes_the_descriptor() {
        let fixture = fixture("drop");
        fixture.runtime.enable().unwrap();
        assert!(fixture.descriptor().is_some());
        let Fixture {
            runtime, scratch, ..
        } = fixture;
        drop(runtime);
        assert!(
            !scratch
                .0
                .join("Hane/integration/whisparrot")
                .join(Descriptor::file_name(std::process::id()))
                .exists()
        );
    }
}
