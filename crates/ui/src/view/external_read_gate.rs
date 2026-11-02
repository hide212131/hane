//! Test-only pause points for the background read of an external-change check
//! (Issue #653, GUI scenarios T03 and T07).
//!
//! A GUI validator cannot see "the read is in flight" or "an old result is
//! about to be applied" from outside: both last for a few milliseconds. Under
//! the dedicated `instrument` build this gate holds a check at one of two
//! stages until the validator releases it, so the state can be observed and
//! the following steps run without sleeping:
//!
//! - [`Stage::Reading`]: the request exists and the file has not been read yet
//!   (T03: start an IME composition, then release; the read sees the latest
//!   disk version).
//! - [`Stage::Applying`]: the file was read and the result is held back before
//!   it reaches the window (T07: hold version B, close the document, open
//!   version C from the same path, then release B and confirm C is kept).
//!
//! The gate is off unless `HANE_EXTERNAL_READ_GATE_DIR` names a directory.
//! Product builds do not compile the call sites (`feature = "instrument"`).
//!
//! Protocol, in that directory, per request `rNNNN` and stage `read` / `apply`:
//!
//! - `rNNNN.<stage>.waiting`: the request is held here. Created before the
//!   check blocks, so seeing it proves the state.
//! - `rNNNN.<stage>.release`: created by the validator to let it continue.
//! - `rNNNN.<stage>.released`: the check continued (the release file is
//!   consumed and the waiting file removed first).
//! - `rNNNN.<stage>.expired`: nobody released it within [`MAX_HOLD`]; the
//!   check continued on its own and the run must not count as evidence.
//!
//! Requests are numbered in the order they start. The user's file path and
//! content are never written; `HANE_EXTERNAL_READ_GATE_FILE` limits the gate to
//! files with that name (the name only, not a path) and
//! `HANE_EXTERNAL_READ_GATE_STAGES` (`read`, `apply` or `read,apply`, default
//! both) picks the stages. Waiting for the release file polls the file system;
//! the poll interval is not part of any pass condition.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

const DIR_VARIABLE: &str = "HANE_EXTERNAL_READ_GATE_DIR";
const FILE_VARIABLE: &str = "HANE_EXTERNAL_READ_GATE_FILE";
const STAGES_VARIABLE: &str = "HANE_EXTERNAL_READ_GATE_STAGES";

/// A held request that nobody releases is let through after this long, so a
/// forgotten run cannot freeze the app for good.
const MAX_HOLD: Duration = Duration::from_secs(300);
const POLL_INTERVAL: Duration = Duration::from_millis(20);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Stage {
    Reading,
    Applying,
}

impl Stage {
    fn name(self) -> &'static str {
        match self {
            Self::Reading => "read",
            Self::Applying => "apply",
        }
    }
}

/// The configured gate, shared by every check of the process.
pub(super) struct ReadGate {
    dir: PathBuf,
    file_name: Option<OsString>,
    reading: bool,
    applying: bool,
    next_request: AtomicU64,
}

/// One admitted check: its identifier and where it may be held.
pub(super) struct GateRequest {
    dir: PathBuf,
    id: String,
    reading: bool,
    applying: bool,
}

/// The gate of this process, or `None` when it is not enabled.
fn active() -> Option<&'static ReadGate> {
    static GATE: OnceLock<Option<ReadGate>> = OnceLock::new();
    GATE.get_or_init(|| {
        ReadGate::new(
            std::env::var_os(DIR_VARIABLE),
            std::env::var_os(FILE_VARIABLE),
            std::env::var(STAGES_VARIABLE).ok().as_deref(),
        )
    })
    .as_ref()
}

/// Admits the check of `path` to the gate, if the gate is on and `path` is the
/// file it was asked to hold.
pub(super) fn admit(path: &Path) -> Option<GateRequest> {
    active()?.admit(path)
}

impl ReadGate {
    fn new(
        dir: Option<OsString>,
        file_name: Option<OsString>,
        stages: Option<&str>,
    ) -> Option<Self> {
        let dir = PathBuf::from(dir.filter(|value| !value.is_empty())?);
        let (reading, applying) = match stages {
            None | Some("") => (true, true),
            Some(list) => (
                list.split(',').any(|item| item.trim() == "read"),
                list.split(',').any(|item| item.trim() == "apply"),
            ),
        };
        Some(Self {
            dir,
            file_name: file_name.filter(|value| !value.is_empty()),
            reading,
            applying,
            next_request: AtomicU64::new(1),
        })
    }

    fn admit(&self, path: &Path) -> Option<GateRequest> {
        if let Some(wanted) = &self.file_name
            && path.file_name() != Some(wanted.as_os_str())
        {
            return None;
        }
        let number = self.next_request.fetch_add(1, Ordering::Relaxed);
        Some(GateRequest {
            dir: self.dir.clone(),
            id: format!("r{number:04}"),
            reading: self.reading,
            applying: self.applying,
        })
    }
}

impl GateRequest {
    /// Blocks the calling (background) thread at `stage` until the validator
    /// releases it. A stage the gate was not asked to hold returns at once.
    pub(super) fn hold(&self, stage: Stage) {
        let enabled = match stage {
            Stage::Reading => self.reading,
            Stage::Applying => self.applying,
        };
        if !enabled {
            return;
        }
        self.announce(stage);
        self.wait_for_release(stage, MAX_HOLD);
    }

    fn marker(&self, stage: Stage, suffix: &str) -> PathBuf {
        self.dir
            .join(format!("{}.{}.{suffix}", self.id, stage.name()))
    }

    /// Makes the held state observable. Best effort: the gate exists only for
    /// a validator that created the directory, and a missing one must not
    /// break the app.
    fn announce(&self, stage: Stage) {
        let _ = std::fs::create_dir_all(&self.dir);
        let _ = std::fs::write(
            self.marker(stage, "waiting"),
            format!("request={}\nstage={}\n", self.id, stage.name()),
        );
    }

    fn wait_for_release(&self, stage: Stage, limit: Duration) {
        let release = self.marker(stage, "release");
        let started = Instant::now();
        let outcome = loop {
            if release.exists() {
                let _ = std::fs::remove_file(&release);
                break "released";
            }
            if started.elapsed() >= limit {
                break "expired";
            }
            std::thread::sleep(POLL_INTERVAL);
        };
        let _ = std::fs::write(self.marker(stage, outcome), outcome);
        let _ = std::fs::remove_file(self.marker(stage, "waiting"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gate_dir(label: &str) -> PathBuf {
        static SEQUENCE: AtomicU64 = AtomicU64::new(0);
        std::env::temp_dir().join(format!(
            "hane-read-gate-{label}-{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ))
    }

    fn gate(dir: &Path, file: Option<&str>, stages: Option<&str>) -> ReadGate {
        ReadGate::new(
            Some(dir.as_os_str().to_owned()),
            file.map(OsString::from),
            stages,
        )
        .expect("a directory enables the gate")
    }

    #[test]
    fn the_gate_is_off_without_a_directory() {
        assert!(ReadGate::new(None, None, None).is_none());
        assert!(ReadGate::new(Some(OsString::new()), None, Some("read")).is_none());
    }

    #[test]
    fn requests_get_ordered_identifiers_that_do_not_contain_the_path() {
        let dir = gate_dir("ids");
        let gate = gate(&dir, None, None);
        let first = gate.admit(Path::new("/secret/notes.md")).expect("admitted");
        let second = gate.admit(Path::new("/secret/notes.md")).expect("admitted");
        assert_eq!(first.id, "r0001");
        assert_eq!(second.id, "r0002");
        assert!(!first.marker(Stage::Reading, "waiting").to_string_lossy().contains("secret"));
    }

    #[test]
    fn only_the_named_file_is_admitted() {
        let dir = gate_dir("file");
        let gate = gate(&dir, Some("held.md"), None);
        assert!(gate.admit(Path::new("/a/other.md")).is_none());
        assert!(gate.admit(Path::new("/a/held.md")).is_some());
        assert!(gate.admit(Path::new("/b/held.md")).is_some());
    }

    #[test]
    fn stages_are_selected_and_an_unselected_stage_does_not_block() {
        let dir = gate_dir("stages");
        let apply_only = gate(&dir, None, Some("apply"));
        let request = apply_only.admit(Path::new("a.md")).expect("admitted");
        // Would block forever if `read` were held: no release file exists.
        request.hold(Stage::Reading);
        assert!(!request.marker(Stage::Reading, "waiting").exists());

        let both = gate(&dir, None, None);
        let request = both.admit(Path::new("a.md")).expect("admitted");
        assert!(request.reading && request.applying);
    }

    #[test]
    fn a_held_request_is_observable_until_it_is_released() {
        let dir = gate_dir("hold");
        let gate = gate(&dir, None, None);
        let request = gate.admit(Path::new("a.md")).expect("admitted");

        request.announce(Stage::Applying);
        let waiting = request.marker(Stage::Applying, "waiting");
        assert!(waiting.exists());
        let text = std::fs::read_to_string(&waiting).expect("marker is readable");
        assert!(text.contains("request=r0001") && text.contains("stage=apply"));
        // The other stage of the same request is not reported as held.
        assert!(!request.marker(Stage::Reading, "waiting").exists());

        std::fs::write(request.marker(Stage::Applying, "release"), "").expect("release");
        request.wait_for_release(Stage::Applying, Duration::from_secs(30));

        assert!(!waiting.exists());
        assert!(!request.marker(Stage::Applying, "release").exists());
        assert!(request.marker(Stage::Applying, "released").exists());
        assert!(!request.marker(Stage::Applying, "expired").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_release_of_one_request_does_not_free_another() {
        let dir = gate_dir("other");
        let gate = gate(&dir, None, None);
        let first = gate.admit(Path::new("a.md")).expect("admitted");
        let second = gate.admit(Path::new("a.md")).expect("admitted");
        second.announce(Stage::Reading);
        std::fs::write(first.marker(Stage::Reading, "release"), "").expect("release");

        // The second request is still held: its own release does not exist, and
        // with no time allowed it ends as expired, not released.
        second.wait_for_release(Stage::Reading, Duration::ZERO);
        assert!(second.marker(Stage::Reading, "expired").exists());
        assert!(!second.marker(Stage::Reading, "released").exists());
        assert!(first.marker(Stage::Reading, "release").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
