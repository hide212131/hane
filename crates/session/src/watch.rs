//! OS change notification for an open work folder.
//!
//! A `FolderWatcher` turns the operating system's native notifications
//! (FSEvents on macOS, `ReadDirectoryChangesW` on Windows, inotify on Linux,
//! all through the `notify` crate) into coalesced `WatchBatch` values. A
//! notification is only a hint that something under the root changed: it never
//! says what the folder looks like now. The receiver re-lists the folder in
//! the background (`WorkFolderScanner::scan`) and installs that result, so a
//! duplicated, reordered, or dropped notification can at worst cost one extra
//! scan, never a wrong list.
//!
//! This module has no GPUI and never opens a file: it only decides *when* a
//! re-list is worth doing. There is no periodic scan; with no notification
//! nothing runs.

use notify::event::ModifyKind;
use notify::{Config, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::ffi::OsStr;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

/// How long the folder must stay quiet before a batch is delivered.
pub const DEFAULT_QUIET_PERIOD: Duration = Duration::from_millis(150);
/// The longest a continuous stream of notifications can postpone a batch, so a
/// folder that never stops changing still refreshes instead of waiting for an
/// end that never comes.
pub const DEFAULT_MAX_WAIT: Duration = Duration::from_millis(500);

/// Debounce timing of one watcher.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WatchTiming {
    pub quiet_period: Duration,
    pub max_wait: Duration,
}

impl Default for WatchTiming {
    fn default() -> Self {
        Self {
            quiet_period: DEFAULT_QUIET_PERIOD,
            max_wait: DEFAULT_MAX_WAIT,
        }
    }
}

/// One coalesced request to re-list the folder.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct WatchBatch {
    /// Set when the notification source itself reported trouble (an overflow
    /// that may have dropped events, a watch error, or the watched root being
    /// removed or renamed). The re-list is still due, and the receiver should
    /// surface this instead of treating the refresh as routine.
    pub failure: Option<String>,
}

enum Signal {
    Changed,
    Failed(String),
    Stop,
}

/// Trailing-edge debounce with a maximum wait. Pure: the caller supplies the
/// clock, so the timing rules are testable without sleeping.
#[derive(Debug)]
struct DebounceWindow {
    timing: WatchTiming,
    first: Option<Instant>,
    last: Option<Instant>,
    failure: Option<String>,
}

impl DebounceWindow {
    fn new(timing: WatchTiming) -> Self {
        Self {
            timing,
            first: None,
            last: None,
            failure: None,
        }
    }

    fn record_change(&mut self, now: Instant) {
        self.first.get_or_insert(now);
        self.last = Some(now);
    }

    fn record_failure(&mut self, now: Instant, message: String) {
        self.record_change(now);
        self.failure = Some(message);
    }

    /// When the pending batch becomes due: after the quiet period, but never
    /// later than `max_wait` past the first unreported notification.
    fn deadline(&self) -> Option<Instant> {
        let first = self.first?;
        let last = self.last?;
        Some((last + self.timing.quiet_period).min(first + self.timing.max_wait))
    }

    fn take_due(&mut self, now: Instant) -> Option<WatchBatch> {
        if self.deadline()? > now {
            return None;
        }
        self.first = None;
        self.last = None;
        Some(WatchBatch {
            failure: self.failure.take(),
        })
    }
}

/// The file names a directory watch reports on. A directory shared by several
/// open files, or by files that are not open, would otherwise wake the
/// receiver for every unrelated write next to them. The set can be replaced
/// while the watch runs, so the same watch follows the files opened and closed
/// in that directory. Names compare case-insensitively: over-matching only
/// costs one extra check, under-matching would lose a change.
#[derive(Clone, Default)]
pub struct WatchedNames(Arc<Mutex<Vec<String>>>);

impl WatchedNames {
    pub fn new(names: impl IntoIterator<Item = impl AsRef<OsStr>>) -> Self {
        let set = Self::default();
        set.set(names);
        set
    }

    pub fn set(&self, names: impl IntoIterator<Item = impl AsRef<OsStr>>) {
        let names: Vec<String> = names
            .into_iter()
            .map(|name| name.as_ref().to_string_lossy().to_lowercase())
            .collect();
        if let Ok(mut current) = self.0.lock() {
            *current = names;
        }
    }

    fn matches(&self, path: &Path) -> bool {
        let Some(name) = path.file_name() else {
            return false;
        };
        let name = name.to_string_lossy().to_lowercase();
        // A poisoned set cannot say what is wanted: report everything.
        self.0
            .lock()
            .map_or(true, |names| names.contains(&name))
    }
}

/// Keeps one folder watched for as long as it lives. Dropping it stops the OS
/// watch and the debounce thread; no batch is delivered afterward.
pub struct FolderWatcher {
    _watcher: RecommendedWatcher,
    control: Sender<Signal>,
}

impl FolderWatcher {
    /// Starts watching `root` recursively. `on_batch` runs on a background
    /// thread (never the caller's) once per coalesced batch and returns
    /// whether the watcher should keep running; returning `false` stops it.
    ///
    /// The watch is established before this returns, so a caller that scans
    /// after `start` cannot miss a change made between the two: that change
    /// arrives as a batch afterward.
    pub fn start(
        root: &Path,
        timing: WatchTiming,
        on_batch: impl FnMut(WatchBatch) -> bool + Send + 'static,
    ) -> io::Result<Self> {
        Self::start_with(root, RecursiveMode::Recursive, None, timing, on_batch)
    }

    /// Starts watching only the entries directly inside `directory`, and only
    /// reports those named in `names` (plus loss of the directory itself and
    /// notification trouble). This is how a file opened outside the work
    /// folder is followed: watching the parent, not the file, is what sees an
    /// editor that saves by replacing the file. Subdirectories are not
    /// watched. The same delivery and release rules as `start` apply.
    pub fn start_directory(
        directory: &Path,
        names: WatchedNames,
        timing: WatchTiming,
        on_batch: impl FnMut(WatchBatch) -> bool + Send + 'static,
    ) -> io::Result<Self> {
        Self::start_with(
            directory,
            RecursiveMode::NonRecursive,
            Some(names),
            timing,
            on_batch,
        )
    }

    fn start_with(
        root: &Path,
        mode: RecursiveMode,
        names: Option<WatchedNames>,
        timing: WatchTiming,
        on_batch: impl FnMut(WatchBatch) -> bool + Send + 'static,
    ) -> io::Result<Self> {
        let (sender, receiver) = mpsc::channel();
        let mut roots = vec![root.to_path_buf()];
        if let Ok(canonical) = root.canonicalize()
            && canonical != root
        {
            roots.push(canonical);
        }
        let handler_sender = sender.clone();
        let mut watcher = RecommendedWatcher::new(
            move |result| {
                if let Some(signal) = classify_for(&roots, names.as_ref(), result) {
                    let _ = handler_sender.send(signal);
                }
            },
            Config::default(),
        )
        .map_err(io::Error::other)?;
        watcher.watch(root, mode).map_err(io::Error::other)?;
        thread::Builder::new()
            .name("hane-folder-watch".to_owned())
            .spawn(move || run_debouncer(&receiver, timing, on_batch))?;
        Ok(Self {
            _watcher: watcher,
            control: sender,
        })
    }
}

impl Drop for FolderWatcher {
    fn drop(&mut self) {
        let _ = self.control.send(Signal::Stop);
    }
}

fn run_debouncer(
    receiver: &Receiver<Signal>,
    timing: WatchTiming,
    mut on_batch: impl FnMut(WatchBatch) -> bool,
) {
    let mut window = DebounceWindow::new(timing);
    loop {
        let received = match window.deadline() {
            None => receiver
                .recv()
                .map_err(|_| RecvTimeoutError::Disconnected),
            Some(deadline) => {
                receiver.recv_timeout(deadline.saturating_duration_since(Instant::now()))
            }
        };
        match received {
            Ok(Signal::Stop) | Err(RecvTimeoutError::Disconnected) => return,
            Ok(Signal::Changed) => window.record_change(Instant::now()),
            Ok(Signal::Failed(message)) => window.record_failure(Instant::now(), message),
            Err(RecvTimeoutError::Timeout) => {}
        }
        if let Some(batch) = window.take_due(Instant::now())
            && !on_batch(batch)
        {
            return;
        }
    }
}

/// `classify`, narrowed to the watched file names when a directory watch has
/// them. Trouble reports and anything about the directory itself always pass.
fn classify_for(
    roots: &[PathBuf],
    names: Option<&WatchedNames>,
    result: notify::Result<Event>,
) -> Option<Signal> {
    if let (Some(names), Ok(event)) = (names, &result)
        && !event.need_rescan()
        && !event.paths.is_empty()
        && !event
            .paths
            .iter()
            .any(|path| roots.contains(path) || names.matches(path))
    {
        return None;
    }
    classify(roots, result)
}

/// Maps one notification to what the debouncer should do about it, or `None`
/// for a notification that cannot change the listing.
fn classify(roots: &[PathBuf], result: notify::Result<Event>) -> Option<Signal> {
    let event = match result {
        Ok(event) => event,
        Err(error) => return Some(Signal::Failed(format!("folder watch error: {error}"))),
    };
    if event.need_rescan() {
        return Some(Signal::Failed(
            "folder notifications were dropped; rescanning".to_owned(),
        ));
    }
    // Reading a note, or the scan itself, is not a change to the listing.
    if matches!(event.kind, EventKind::Access(_)) {
        return None;
    }
    let root_affected = event.paths.iter().any(|path| roots.contains(path));
    if root_affected
        && matches!(
            event.kind,
            EventKind::Remove(_)
                | EventKind::Modify(ModifyKind::Name(_))
        )
    {
        return Some(Signal::Failed(
            "the work folder was removed or renamed".to_owned(),
        ));
    }
    // `.hane` directly under the root is Hane's own state (the draft
    // journal), never listed; writing a draft must not cause a re-list.
    if !event.paths.is_empty()
        && event
            .paths
            .iter()
            .all(|path| roots.iter().any(|root| path.starts_with(root.join(".hane"))))
    {
        return None;
    }
    Some(Signal::Changed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service::temporary_directory;
    use notify::event::{AccessKind, CreateKind, Flag, RemoveKind, RenameMode};
    use std::fs;

    /// Upper bound the real-filesystem tests wait for a single change to be
    /// reported. It covers the debounce (at most `DEFAULT_MAX_WAIT`) plus the
    /// operating system's own notification latency, which is not under this
    /// module's control and is deliberately given a wide margin.
    const NOTIFICATION_WAIT_LIMIT: Duration = Duration::from_secs(10);

    fn timing(quiet_ms: u64, max_ms: u64) -> WatchTiming {
        WatchTiming {
            quiet_period: Duration::from_millis(quiet_ms),
            max_wait: Duration::from_millis(max_ms),
        }
    }

    #[test]
    fn a_single_change_is_due_after_the_quiet_period() {
        let start = Instant::now();
        let mut window = DebounceWindow::new(timing(100, 500));
        assert!(window.deadline().is_none());
        window.record_change(start);
        assert!(window.take_due(start + Duration::from_millis(99)).is_none());
        assert_eq!(
            window.take_due(start + Duration::from_millis(100)),
            Some(WatchBatch::default())
        );
        assert!(window.take_due(start + Duration::from_secs(10)).is_none());
    }

    #[test]
    fn repeated_changes_extend_the_quiet_period() {
        let start = Instant::now();
        let mut window = DebounceWindow::new(timing(100, 1000));
        window.record_change(start);
        window.record_change(start + Duration::from_millis(80));
        assert!(
            window
                .take_due(start + Duration::from_millis(150))
                .is_none()
        );
        assert!(
            window
                .take_due(start + Duration::from_millis(180))
                .is_some()
        );
    }

    #[test]
    fn a_continuous_stream_is_still_reported_by_the_max_wait() {
        let start = Instant::now();
        let mut window = DebounceWindow::new(timing(100, 500));
        for step in 0..20 {
            window.record_change(start + Duration::from_millis(step * 40));
        }
        // The last change was at 760ms, so the quiet period alone would wait
        // until 860ms; the max wait from the first change (500ms) wins.
        assert_eq!(
            window.deadline(),
            Some(start + Duration::from_millis(500))
        );
        assert!(
            window
                .take_due(start + Duration::from_millis(500))
                .is_some()
        );
    }

    #[test]
    fn a_failure_is_reported_once_with_the_next_batch() {
        let start = Instant::now();
        let mut window = DebounceWindow::new(timing(10, 50));
        window.record_failure(start, "overflow".to_owned());
        let batch = window.take_due(start + Duration::from_millis(10)).unwrap();
        assert_eq!(batch.failure.as_deref(), Some("overflow"));
        window.record_change(start + Duration::from_millis(20));
        let batch = window.take_due(start + Duration::from_millis(40)).unwrap();
        assert_eq!(batch.failure, None);
    }

    fn roots() -> Vec<PathBuf> {
        vec![PathBuf::from("/notes")]
    }

    fn event(kind: EventKind, path: &str) -> notify::Result<Event> {
        Ok(Event::new(kind).add_path(PathBuf::from(path)))
    }

    #[test]
    fn creates_removes_and_modifications_are_hints_to_rescan() {
        for kind in [
            EventKind::Create(CreateKind::File),
            EventKind::Remove(RemoveKind::File),
            EventKind::Modify(ModifyKind::Any),
            EventKind::Modify(ModifyKind::Name(RenameMode::Both)),
        ] {
            assert!(matches!(
                classify(&roots(), event(kind, "/notes/a/B.md")),
                Some(Signal::Changed)
            ));
        }
    }

    #[test]
    fn access_events_and_hane_state_changes_are_ignored() {
        assert!(
            classify(
                &roots(),
                event(EventKind::Access(AccessKind::Any), "/notes/A.md")
            )
            .is_none()
        );
        assert!(
            classify(
                &roots(),
                event(
                    EventKind::Create(CreateKind::File),
                    "/notes/.hane/drafts/1.md"
                )
            )
            .is_none()
        );
        // A `.hane` further down is the user's own folder and still counts.
        assert!(matches!(
            classify(
                &roots(),
                event(EventKind::Create(CreateKind::File), "/notes/x/.hane/A.md")
            ),
            Some(Signal::Changed)
        ));
    }

    #[test]
    fn overflow_errors_and_root_loss_are_reported_as_failures() {
        let overflow = Ok(Event::new(EventKind::Other)
            .add_path(PathBuf::from("/notes"))
            .set_flag(Flag::Rescan));
        assert!(matches!(
            classify(&roots(), overflow),
            Some(Signal::Failed(_))
        ));
        assert!(matches!(
            classify(
                &roots(),
                Err(notify::Error::generic("watch failed"))
            ),
            Some(Signal::Failed(_))
        ));
        assert!(matches!(
            classify(&roots(), event(EventKind::Remove(RemoveKind::Folder), "/notes")),
            Some(Signal::Failed(_))
        ));
        // A child removal is an ordinary change, not root loss.
        assert!(matches!(
            classify(
                &roots(),
                event(EventKind::Remove(RemoveKind::Folder), "/notes/child")
            ),
            Some(Signal::Changed)
        ));
    }

    fn start_collecting(root: &Path) -> (FolderWatcher, mpsc::Receiver<WatchBatch>) {
        let (sender, receiver) = mpsc::channel();
        let watcher = FolderWatcher::start(root, timing(50, 200), move |batch| {
            sender.send(batch).is_ok()
        })
        .unwrap();
        (watcher, receiver)
    }

    #[test]
    fn an_external_change_to_a_real_folder_is_reported() {
        let root = temporary_directory("watch-change");
        fs::create_dir_all(root.join("nested")).unwrap();
        let (watcher, receiver) = start_collecting(&root);

        fs::write(root.join("nested/New.md"), "# New\n").unwrap();
        let batch = receiver
            .recv_timeout(NOTIFICATION_WAIT_LIMIT)
            .expect("a nested file creation is reported within the wait limit");
        assert_eq!(batch.failure, None);

        drop(watcher);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn nothing_is_reported_while_the_folder_is_idle() {
        let root = temporary_directory("watch-idle");
        fs::create_dir_all(&root).unwrap();
        let (watcher, receiver) = start_collecting(&root);
        // Setting the folder up can itself be reported late by the OS; let
        // that settle so only changes made after this point are measured.
        while receiver.recv_timeout(Duration::from_millis(500)).is_ok() {}
        assert!(receiver.recv_timeout(Duration::from_millis(600)).is_err());
        drop(watcher);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_dropped_watcher_delivers_nothing_further() {
        let root = temporary_directory("watch-drop");
        fs::create_dir_all(&root).unwrap();
        let (watcher, receiver) = start_collecting(&root);
        drop(watcher);

        fs::write(root.join("After.md"), "# After\n").unwrap();
        assert!(
            receiver.recv_timeout(Duration::from_millis(800)).is_err(),
            "a released watch must not report later changes"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_directory_watch_reports_only_the_watched_names_and_trouble() {
        let roots = vec![PathBuf::from("/notes")];
        let names = WatchedNames::new(["Note.md"]);
        let create = || EventKind::Create(CreateKind::File);
        assert!(matches!(
            classify_for(&roots, Some(&names), event(create(), "/notes/note.MD")),
            Some(Signal::Changed)
        ));
        assert!(classify_for(&roots, Some(&names), event(create(), "/notes/Other.md")).is_none());
        // An atomic replacement names the temporary file and the target.
        let replace = Ok(Event::new(EventKind::Modify(ModifyKind::Name(RenameMode::Both)))
            .add_path(PathBuf::from("/notes/.tmp123"))
            .add_path(PathBuf::from("/notes/Note.md")));
        assert!(matches!(
            classify_for(&roots, Some(&names), replace),
            Some(Signal::Changed)
        ));
        // Loss of the directory and notification trouble are never filtered.
        assert!(matches!(
            classify_for(
                &roots,
                Some(&names),
                event(EventKind::Remove(RemoveKind::Folder), "/notes")
            ),
            Some(Signal::Failed(_))
        ));
        assert!(matches!(
            classify_for(
                &roots,
                Some(&names),
                Err(notify::Error::generic("watch failed"))
            ),
            Some(Signal::Failed(_))
        ));
        let overflow = Ok(Event::new(EventKind::Other)
            .add_path(PathBuf::from("/elsewhere"))
            .set_flag(Flag::Rescan));
        assert!(matches!(
            classify_for(&roots, Some(&names), overflow),
            Some(Signal::Failed(_))
        ));
        // The set follows the files opened and closed meanwhile.
        names.set(["Other.md"]);
        assert!(matches!(
            classify_for(&roots, Some(&names), event(create(), "/notes/Other.md")),
            Some(Signal::Changed)
        ));
        assert!(classify_for(&roots, Some(&names), event(create(), "/notes/Note.md")).is_none());
    }

    #[test]
    fn a_directory_watch_sees_an_atomic_replacement_of_a_watched_file_only() {
        let root = temporary_directory("watch-directory");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("Note.md"), "# Note\n").unwrap();
        let (sender, receiver) = mpsc::channel();
        let watcher = FolderWatcher::start_directory(
            &root,
            WatchedNames::new(["Note.md"]),
            timing(50, 200),
            move |batch| sender.send(batch).is_ok(),
        )
        .unwrap();
        // Setting the folder up can itself be reported late by the OS.
        while receiver.recv_timeout(Duration::from_millis(500)).is_ok() {}

        fs::write(root.join("Unrelated.md"), "# Unrelated\n").unwrap();
        assert!(
            receiver.recv_timeout(Duration::from_millis(800)).is_err(),
            "a file that is not watched must not wake the receiver"
        );

        fs::write(root.join(".replacement"), "# Replaced\n").unwrap();
        fs::rename(root.join(".replacement"), root.join("Note.md")).unwrap();
        let batch = receiver
            .recv_timeout(NOTIFICATION_WAIT_LIMIT)
            .expect("replacing the watched file is reported within the wait limit");
        assert_eq!(batch.failure, None);

        drop(watcher);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn watching_a_missing_folder_is_an_error() {
        let root = temporary_directory("watch-missing");
        assert!(FolderWatcher::start(&root, WatchTiming::default(), |_| true).is_err());
    }
}
