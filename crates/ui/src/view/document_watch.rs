//! Keeps every open, saved document watched for changes made outside the app
//! (Issue #653).
//!
//! The work folder already has one recursive watch (`folder_watch`). A file
//! inside that root is covered by it and is never registered a second time. A
//! file anywhere else is followed through a non-recursive watch on its parent
//! directory, because an editor that saves by replacing the file is only seen
//! from there. Documents that share a directory share one watch, which only
//! reports the names of the files that are open, so an unrelated write next to
//! them wakes nothing and no directory is watched recursively for the sake of
//! one file.
//!
//! A notification is a hint. It starts the same background check as every
//! other trigger (`external_change`): at most one job per document plus one
//! follow-up, whose result is accepted only for the document instance and
//! ticket it was started for. Nothing here reads or compares a file on the UI
//! thread, and nothing polls: with no notification, no return to the app and
//! no failed watch to retry, nothing runs.
//!
//! What is watched is derived, not hand-maintained. `sync_document_watches`
//! compares the open documents with the registered watches whenever the view
//! renders (a path-set comparison, no I/O) and registers, updates or releases
//! watches to match, so open, close, rename, save-as, re-open and a work folder
//! switch all end in the same state without each of them having to remember to
//! call it. The last document that needs a directory releases its watch, and
//! the task that receives its notifications ends with it.
//!
//! A watch that cannot be started, or that reports trouble (overflow, an error,
//! the directory removed), is never treated as healthy: the open documents are
//! checked again, the watch is re-established, a failed attempt is retried
//! by one timer, and the documents it was meant to cover say so in the footer
//! with a manual "check again" choice until it is back.

use super::*;
#[cfg(not(test))]
use hane_session::WatchTiming;
use hane_session::{FolderWatcher, WatchBatch, WatchedNames};
use std::ffi::OsString;

/// How long to wait before registering a failed directory watch again.
/// Retrying is timer-driven (one timer at a time) and does not read any file.
const DOCUMENT_WATCH_RETRY_DELAY: Duration = Duration::from_secs(5);

#[cfg(test)]
thread_local! {
    /// Test-only error injection: the next directory watches fail with this
    /// message instead of being registered.
    static INJECTED_DOCUMENT_WATCH_ERROR: std::cell::RefCell<Option<String>> =
        const { std::cell::RefCell::new(None) };
    /// Test-only: registrations wait in `HELD_DOCUMENT_WATCH_REGISTRATIONS`
    /// instead of finishing, standing in for a slow directory.
    static HOLD_DOCUMENT_WATCH_REGISTRATION: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
    static HELD_DOCUMENT_WATCH_REGISTRATIONS: std::cell::RefCell<Vec<HeldRegistration>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

#[cfg(test)]
type HeldRegistration = (PathBuf, u64, Result<Option<FolderWatcher>, String>);

/// One registered (or failed) watch on the parent directory of open files.
struct ParentWatch {
    /// Tells this registration apart from a later one for the same directory;
    /// a notification carrying an older token is dropped.
    token: u64,
    /// The file names open in this directory right now.
    names: WatchedNames,
    /// Held only for its lifetime: dropping it releases the OS watch.
    _watcher: Option<FolderWatcher>,
    /// Test-only stand-in for the watcher thread's sender; dropping it closes
    /// the channel just as dropping the watcher does.
    #[cfg(test)]
    test_batches: Option<async_channel::Sender<WatchBatch>>,
    /// Why the directory is not being watched; `None` when it is.
    failure: Option<String>,
    /// The OS watcher is still being created off the UI thread. Such a watch
    /// is neither healthy nor failed.
    pending: bool,
    /// Look at the directory's documents once the registration succeeds.
    check_on_ready: bool,
    /// Look at them even if the registration fails.
    check_when_failed: bool,
}

/// What the watch lifecycle knows about one open saved document.
struct TrackedDocument {
    /// The paths whose directories report on this document: the path as
    /// opened and, for a symlink, the path it resolves to.
    targets: Vec<PathBuf>,
    /// The first check after this path was opened (or renamed) has not been
    /// started yet because nothing watches the file so far.
    initial_pending: bool,
}

/// Watch bookkeeping for the open documents. Dropped with the view.
#[derive(Default)]
pub(super) struct DocumentWatchState {
    /// The work folder generation the entries below belong to; a new one
    /// discards them all, so nothing from the old workspace survives.
    epoch: u64,
    parents: HashMap<PathBuf, ParentWatch>,
    tracked: HashMap<SessionId, TrackedDocument>,
    next_token: u64,
    /// A delayed re-registration is already waiting, so failures never stack
    /// timers.
    retry_scheduled: bool,
    /// Registered once, the first time the window is available.
    activation: Option<Subscription>,
}

/// The directory whose watch reports on `path`.
fn watch_parent(path: &Path) -> Option<PathBuf> {
    let parent = path.parent()?;
    Some(if parent.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        parent.to_path_buf()
    })
}

/// The paths to watch for `session`: the path it was opened with, plus the
/// path it resolves to when that is somewhere else (a symlink). The resolved
/// path comes from the identity recorded when the file was opened, so no
/// filesystem access happens here. A change to the link target is reported in
/// the target's directory, which the link's own directory never sees.
fn watch_targets(session: &DocumentSession) -> Vec<PathBuf> {
    let Some(identity) = session.file().identity() else {
        return Vec::new();
    };
    let path = identity.path();
    let mut targets = vec![path.to_path_buf()];
    let resolved = identity.canonical_path();
    if resolved != path && !is_private_alias(path, resolved) {
        // A target in the link's own directory is reached through the same
        // directory spelling, so both names share one watch even when the
        // resolved path spells that directory as `/private/...`.
        let in_same_directory = match (path.parent(), resolved.parent(), resolved.file_name()) {
            (Some(own), Some(resolved_parent), Some(name))
                if own == resolved_parent || is_private_alias(own, resolved_parent) =>
            {
                Some(own.join(name))
            }
            _ => None,
        };
        targets.push(in_same_directory.unwrap_or_else(|| resolved.to_path_buf()));
    }
    targets
}

/// macOS resolves `/var` to `/private/var`; that is the same file under the
/// same directory watch, not a link to follow separately.
fn is_private_alias(path: &Path, resolved: &Path) -> bool {
    resolved
        .strip_prefix("/private")
        .is_ok_and(|rest| path.strip_prefix("/").is_ok_and(|plain| plain == rest))
}

/// Whether `parent` is the directory of any watch target of `session`.
fn uses_parent(session: &DocumentSession, parent: &Path) -> bool {
    watch_targets(session)
        .iter()
        .any(|target| watch_parent(target).as_deref() == Some(parent))
}

impl EditorView {
    /// Why the file of `id` is not being followed, or `None` when it is (or
    /// when it has no file). A document inside a healthy work folder is
    /// covered by the folder watch.
    pub(super) fn document_watch_problem(&self, id: SessionId) -> Option<String> {
        watch_targets(self.sessions.get(id)?)
            .iter()
            .filter(|target| !self.folder_watch_covers(target))
            .filter_map(|target| watch_parent(target))
            .find_map(|parent| self.document_watch.parents.get(&parent)?.failure.clone())
    }

    /// Whether the active document's changes are currently not followed.
    pub(super) fn active_document_watch_problem(&self) -> bool {
        self.document_watch_problem(self.sessions.active_id())
            .is_some()
    }

    /// One warning per document that is not being followed, the active one
    /// first. It lives as long as the problem does; switching tabs or later
    /// status messages never hide it.
    pub(super) fn document_watch_warnings(&self) -> Vec<String> {
        let active = self.sessions.active_id();
        let mut ordered: Vec<&DocumentSession> = self.sessions.sessions().collect();
        ordered.sort_by_key(|session| session.id() != active);
        ordered
            .into_iter()
            .filter_map(|session| {
                let reason = self.document_watch_problem(session.id())?;
                Some(format!(
                    "{}: 外部での変更を自動では検知できません（{reason}）。ディスクの内容を保証できないため、保存状態のメニューの「ディスクの内容を再確認」で確認してください。",
                    session.label()
                ))
            })
            .collect()
    }

    /// Registers the activation observer once. The window is only available
    /// from the render and event paths, so the first render installs it; it is
    /// released with the view.
    pub(super) fn observe_window_return(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.document_watch.activation.is_some() {
            return;
        }
        self.document_watch.activation =
            Some(cx.observe_window_activation(window, |view, window, cx| {
                if window.is_window_active() {
                    view.recheck_documents_on_return(cx);
                }
            }));
    }

    /// The app came back to the front: notifications may have been missed
    /// while it was away, so every open file is looked at once, and watches
    /// that failed meanwhile are tried again.
    pub(super) fn recheck_documents_on_return(&mut self, cx: &mut Context<Self>) {
        self.retry_failed_document_watches(cx);
        self.check_open_documents_for_external_changes(cx);
    }

    /// Brings the registered watches in line with the open documents: starts
    /// the ones that are missing, updates the file names of those that stay,
    /// releases the ones nobody needs, and starts the first check of every
    /// document that is now watched. Cheap when nothing changed.
    pub(super) fn sync_document_watches(&mut self, cx: &mut Context<Self>) {
        if self.document_watch.epoch != self.work_folder_generation {
            // Another workspace: nothing registered for the old one applies.
            self.document_watch.parents.clear();
            self.document_watch.tracked.clear();
            self.document_watch.epoch = self.work_folder_generation;
        }
        let documents: Vec<(SessionId, Vec<PathBuf>)> = self
            .sessions
            .sessions()
            .map(|session| (session.id(), watch_targets(session)))
            .filter(|(_, targets)| !targets.is_empty())
            .collect();
        self.document_watch
            .tracked
            .retain(|id, _| documents.iter().any(|(open, _)| open == id));
        for (id, targets) in &documents {
            let current = self
                .document_watch
                .tracked
                .get(id)
                .is_some_and(|tracked| tracked.targets == *targets);
            if !current {
                self.document_watch.tracked.insert(
                    *id,
                    TrackedDocument {
                        targets: targets.clone(),
                        initial_pending: true,
                    },
                );
            }
        }

        let mut wanted: HashMap<PathBuf, Vec<OsString>> = HashMap::new();
        for (_, targets) in &documents {
            for path in targets {
                if self.folder_watch_covers(path) {
                    continue;
                }
                let (Some(parent), Some(name)) = (watch_parent(path), path.file_name()) else {
                    continue;
                };
                wanted.entry(parent).or_default().push(name.to_os_string());
            }
        }
        self.document_watch
            .parents
            .retain(|parent, _| wanted.contains_key(parent));
        for (parent, names) in &wanted {
            if let Some(existing) = self.document_watch.parents.get(parent) {
                existing.names.set(names);
                continue;
            }
            let set = WatchedNames::new(names);
            self.attach_document_watch(parent, &set, false, false, cx);
        }
        self.start_initial_document_checks(cx);
    }

    /// Starts the first check of every document whose watch is registered. It
    /// comes after the registration, so a change between the two arrives as a
    /// notification instead of being lost. A registration that is still
    /// pending (it runs off the UI thread) or failed does not count.
    fn start_initial_document_checks(&mut self, cx: &mut Context<Self>) {
        let pending: Vec<(SessionId, Vec<PathBuf>)> = self
            .document_watch
            .tracked
            .iter()
            .filter(|(_, tracked)| tracked.initial_pending)
            .map(|(id, tracked)| (*id, tracked.targets.clone()))
            .collect();
        for (id, targets) in pending {
            let watched = targets.iter().all(|path| {
                self.folder_watch_covers(path)
                    || watch_parent(path)
                        .and_then(|parent| self.document_watch.parents.get(&parent))
                        .is_some_and(|watch| !watch.pending && watch.failure.is_none())
            });
            if !watched {
                continue;
            }
            if let Some(tracked) = self.document_watch.tracked.get_mut(&id) {
                tracked.initial_pending = false;
            }
            self.start_external_check(id, cx);
        }
    }

    /// Records a new registration for `parent` right away (as pending) and
    /// starts the task that delivers its notifications. Creating the OS
    /// watcher can wait on a slow or network directory, so it never runs on
    /// the UI thread: `complete_document_watch` applies the outcome later.
    /// `check_on_ready` looks at the directory's documents once the watch is
    /// registered; `check_when_failed` also does so when it could not be.
    fn attach_document_watch(
        &mut self,
        parent: &Path,
        names: &WatchedNames,
        check_on_ready: bool,
        check_when_failed: bool,
        cx: &mut Context<Self>,
    ) {
        self.document_watch.next_token = self.document_watch.next_token.wrapping_add(1);
        let token = self.document_watch.next_token;
        let (sender, receiver) = async_channel::unbounded();
        // GPUI's deterministic test scheduler must not be woken by a real OS
        // thread, so unit tests do not start the OS watcher. They feed batches
        // through `test_batches`; the OS watcher itself is covered by the
        // `hane_session` tests.
        #[cfg(test)]
        let watch_sender = sender.clone();
        let watch = ParentWatch {
            token,
            names: names.clone(),
            _watcher: None,
            #[cfg(test)]
            test_batches: Some(watch_sender),
            failure: None,
            pending: true,
            check_on_ready,
            check_when_failed,
        };
        self.document_watch
            .parents
            .insert(parent.to_path_buf(), watch);

        let parent = parent.to_path_buf();
        {
            let parent = parent.clone();
            cx.spawn(async move |view, cx| {
                // Ends when the watch is released (the channel closes), the
                // view is gone, or this registration was replaced.
                while let Ok(batch) = receiver.recv().await {
                    let keep_going = view
                        .update(cx, |view, cx| {
                            view.document_watch_batch_arrived(&parent, token, batch, cx)
                        })
                        .unwrap_or(false);
                    if !keep_going {
                        break;
                    }
                }
            })
            .detach();
        }

        #[cfg(not(test))]
        {
            let names = names.clone();
            cx.spawn(async move |view, cx| {
                let directory = parent.clone();
                let outcome = cx
                    .background_executor()
                    .spawn(async move {
                        FolderWatcher::start_directory(
                            &directory,
                            names,
                            WatchTiming::default(),
                            move |batch| sender.send_blocking(batch).is_ok(),
                        )
                        .map(Some)
                        .map_err(|error| error.to_string())
                    })
                    .await;
                let _ = view.update(cx, |view, cx| {
                    view.complete_document_watch(&parent, token, outcome, cx);
                });
            })
            .detach();
        }
        #[cfg(test)]
        {
            drop(sender);
            let outcome = match INJECTED_DOCUMENT_WATCH_ERROR.with(|slot| slot.borrow().clone()) {
                Some(message) => Err(message),
                None => Ok(None),
            };
            if HOLD_DOCUMENT_WATCH_REGISTRATION.with(std::cell::Cell::get) {
                HELD_DOCUMENT_WATCH_REGISTRATIONS
                    .with(|held| held.borrow_mut().push((parent, token, outcome)));
            } else {
                self.complete_document_watch(&parent, token, outcome, cx);
            }
        }
    }

    /// Applies the outcome of a registration that ran off the UI thread. It is
    /// accepted only while the registration is still the current one for a
    /// directory some open document needs in this work folder generation;
    /// otherwise the watcher is dropped (released) and nothing is checked.
    fn complete_document_watch(
        &mut self,
        parent: &Path,
        token: u64,
        outcome: Result<Option<FolderWatcher>, String>,
        cx: &mut Context<Self>,
    ) {
        let is_current = self
            .document_watch
            .parents
            .get(parent)
            .is_some_and(|watch| watch.token == token && watch.pending);
        if !is_current {
            return;
        }
        let still_needed = self.document_watch.epoch == self.work_folder_generation
            && self
                .sessions
                .sessions()
                .any(|session| uses_parent(session, parent));
        if !still_needed {
            // The document closed, moved or the work folder changed while the
            // registration ran. Forget the entry so a later sync registers
            // afresh; the dropped outcome releases the watcher.
            self.document_watch.parents.remove(parent);
            return;
        }
        let Some(watch) = self.document_watch.parents.get_mut(parent) else {
            return;
        };
        watch.pending = false;
        let (check_on_ready, check_when_failed) = (watch.check_on_ready, watch.check_when_failed);
        let failed = match outcome {
            Ok(watcher) => {
                watch._watcher = watcher;
                false
            }
            Err(message) => {
                watch.failure = Some(message);
                #[cfg(test)]
                {
                    watch.test_batches = None;
                }
                true
            }
        };
        if failed {
            self.schedule_document_watch_retry(cx);
        }
        if (!failed && check_on_ready) || (failed && check_when_failed) {
            self.check_documents_in(parent, cx);
        }
        self.start_initial_document_checks(cx);
        cx.notify();
    }

    /// Returns whether the registration this batch came from is still current.
    fn document_watch_batch_arrived(
        &mut self,
        parent: &Path,
        token: u64,
        batch: WatchBatch,
        cx: &mut Context<Self>,
    ) -> bool {
        let current = self
            .document_watch
            .parents
            .get(parent)
            .is_some_and(|watch| watch.token == token);
        if !current {
            return false;
        }
        if batch.failure.is_some() {
            // Overflow, a notification error or a lost directory: what the
            // source says next cannot be trusted to cover what it missed.
            self.reattach_document_watch(parent, true, cx);
            return false;
        }
        self.check_documents_in(parent, cx);
        true
    }

    /// Replaces the registration for `parent` and looks at its documents.
    /// `check_when_failed` also looks when the new registration failed (the
    /// caller has fresh evidence something may have changed); a periodic retry
    /// passes `false`, so retrying never reads files by itself.
    fn reattach_document_watch(
        &mut self,
        parent: &Path,
        check_when_failed: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(old) = self.document_watch.parents.remove(parent) else {
            return;
        };
        let names = old.names.clone();
        // Released first so the old task ends instead of two registrations
        // reporting the same directory.
        drop(old);
        // The documents are checked once the new registration finishes (or
        // fails, when `check_when_failed`), not before the watch exists.
        self.attach_document_watch(parent, &names, true, check_when_failed, cx);
        cx.notify();
    }

    /// Starts the background check of every open document in `parent`. Files
    /// that are not open are never read.
    fn check_documents_in(&mut self, parent: &Path, cx: &mut Context<Self>) {
        let ids: Vec<SessionId> = self
            .sessions
            .sessions()
            .filter(|session| uses_parent(session, parent))
            .map(DocumentSession::id)
            .collect();
        for id in ids {
            if let Some(tracked) = self.document_watch.tracked.get_mut(&id) {
                tracked.initial_pending = false;
            }
            self.start_external_check(id, cx);
        }
    }

    fn schedule_document_watch_retry(&mut self, cx: &mut Context<Self>) {
        if self.document_watch.retry_scheduled {
            return;
        }
        self.document_watch.retry_scheduled = true;
        cx.spawn(async move |view, cx| {
            cx.background_executor()
                .timer(DOCUMENT_WATCH_RETRY_DELAY)
                .await;
            let _ = view.update(cx, |view, cx| {
                view.document_watch.retry_scheduled = false;
                view.retry_failed_document_watches(cx);
            });
        })
        .detach();
    }

    /// Registers again every watch that failed. A watch that comes back gets
    /// its documents checked, because changes made while it was down were never
    /// reported; one that fails again waits for the next timer.
    fn retry_failed_document_watches(&mut self, cx: &mut Context<Self>) {
        let failed: Vec<PathBuf> = self
            .document_watch
            .parents
            .iter()
            .filter(|(_, watch)| watch.failure.is_some())
            .map(|(parent, _)| parent.clone())
            .collect();
        if failed.is_empty() {
            return;
        }
        for parent in failed {
            self.reattach_document_watch(&parent, false, cx);
        }
    }

    /// The user's "check again": tries the watch again if it is down, then
    /// reads the active document's file now.
    pub(super) fn recheck_active_document(&mut self, cx: &mut Context<Self>) {
        let id = self.sessions.active_id();
        if self.sessions.active().path().is_none() {
            return;
        }
        let down = self.sessions.get(id).and_then(|session| {
            watch_targets(session)
                .iter()
                .filter_map(|target| watch_parent(target))
                .find(|parent| {
                    self.document_watch
                        .parents
                        .get(parent)
                        .is_some_and(|watch| watch.failure.is_some())
                })
        });
        self.status = Some("ディスクの内容を再確認しています".to_owned());
        match down {
            Some(parent) => self.reattach_document_watch(&parent, true, cx),
            None => self.start_external_check(id, cx),
        }
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::super::folder_watch::inject_folder_attach_error;
    use super::*;

    fn watch_test_root(label: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQUENCE: AtomicU64 = AtomicU64::new(0);
        std::env::temp_dir().join(format!(
            "hane-document-watch-{label}-{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ))
    }

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    /// A view (no window, so nothing renders by itself) with the files open as
    /// tabs, the last one active.
    fn view_with_files(
        cx: &mut gpui::TestAppContext,
        paths: &[&Path],
    ) -> gpui::Entity<EditorView> {
        let first = OsFileService.load(paths[0]).unwrap();
        let view = gpui::AppContext::new(cx, |cx| {
            EditorView::from_sessions(
                SessionSet::with_loaded(first),
                Arc::new(OsFileService),
                StateStores::memory(),
                cx,
            )
        });
        for path in &paths[1..] {
            let loaded = OsFileService.load(path).unwrap();
            view.update(cx, |view, _| {
                view.sessions.apply_open(None, loaded);
            });
        }
        view
    }

    fn inject_watch_error(message: Option<&str>) {
        INJECTED_DOCUMENT_WATCH_ERROR.with(|slot| *slot.borrow_mut() = message.map(str::to_owned));
    }

    fn ids(view: &gpui::Entity<EditorView>, cx: &gpui::TestAppContext) -> Vec<SessionId> {
        view.read_with(cx, |view, _| {
            view.sessions.sessions().map(DocumentSession::id).collect()
        })
    }

    fn text_of(view: &gpui::Entity<EditorView>, cx: &gpui::TestAppContext, id: SessionId) -> String {
        view.read_with(cx, |view, _| {
            view.sessions
                .get(id)
                .unwrap()
                .editor()
                .document()
                .full_text()
        })
    }

    fn only_batch_sender(
        view: &gpui::Entity<EditorView>,
        cx: &gpui::TestAppContext,
    ) -> async_channel::Sender<WatchBatch> {
        view.read_with(cx, |view, _| {
            assert_eq!(view.document_watch.parents.len(), 1);
            view.document_watch
                .parents
                .values()
                .next()
                .unwrap()
                .test_batches
                .clone()
                .expect("the directory watch is registered")
        })
    }

    fn only_token(view: &gpui::Entity<EditorView>, cx: &gpui::TestAppContext) -> u64 {
        view.read_with(cx, |view, _| {
            view.document_watch.parents.values().next().unwrap().token
        })
    }

    #[gpui::test]
    fn open_files_share_one_parent_watch_and_are_checked_after_it_is_registered(
        cx: &mut gpui::TestAppContext,
    ) {
        inject_watch_error(None);
        let root = watch_test_root("shared");
        let (a, b, unopened) = (root.join("A.md"), root.join("B.md"), root.join("C.md"));
        write(&a, "a\n");
        write(&b, "b\n");
        write(&unopened, "c\n");
        let view = view_with_files(cx, &[&a, &b]);
        let ids = ids(&view, cx);
        // Both files change after they were opened and before any watch exists.
        write(&a, "a changed\n");
        write(&b, "b changed\n");
        view.update(cx, |view, cx| {
            // An untitled tab has no file: it is not tracked or watched.
            view.sessions.open_untitled("draft", "Untitled");
            view.sync_document_watches(cx);
            view.sync_document_watches(cx);
            assert_eq!(view.document_watch.parents.len(), 1, "one shared parent");
            assert_eq!(view.document_watch.tracked.len(), 2);
        });
        cx.run_until_parked();

        assert_eq!(text_of(&view, cx, ids[0]), "a changed\n");
        assert_eq!(text_of(&view, cx, ids[1]), "b changed\n");
        only_batch_sender(&view, cx);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Not run on Windows: creating a symlink there needs a privilege the
    /// environment may not grant, so this coverage is Unix-only.
    #[cfg(unix)]
    fn symlinked_view(
        cx: &mut gpui::TestAppContext,
        label: &str,
        target_dir: &str,
        target_name: &str,
    ) -> (gpui::Entity<EditorView>, PathBuf, PathBuf) {
        let root = watch_test_root(label);
        let target = root.join(target_dir).join(target_name);
        let link = root.join("links/Note.md");
        write(&target, "target\n");
        std::fs::create_dir_all(link.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let view = view_with_files(cx, &[&link]);
        (view, root, target)
    }

    #[cfg(unix)]
    #[gpui::test]
    fn a_symlink_target_in_another_directory_is_watched_and_checked(
        cx: &mut gpui::TestAppContext,
    ) {
        inject_watch_error(None);
        let (view, root, target) = symlinked_view(cx, "link-other-dir", "real", "Real.md");
        let id = ids(&view, cx)[0];
        view.update(cx, |view, cx| view.sync_document_watches(cx));
        cx.run_until_parked();
        let target_parent = std::fs::canonicalize(&target)
            .unwrap()
            .parent()
            .unwrap()
            .to_path_buf();
        let (link_sender, target_sender) = view.read_with(cx, |view, _| {
            assert_eq!(view.document_watch.parents.len(), 2);
            let link_parent = view
                .document_watch
                .parents
                .keys()
                .find(|parent| **parent != target_parent)
                .expect("the link's own directory is watched")
                .clone();
            (
                view.document_watch.parents[&link_parent]
                    .test_batches
                    .clone()
                    .unwrap(),
                view.document_watch.parents[&target_parent]
                    .test_batches
                    .clone()
                    .expect("the resolved target's directory is watched"),
            )
        });
        // The display path stays the link; only the watch follows the target.
        view.read_with(cx, |view, _| {
            assert!(
                view.sessions
                    .get(id)
                    .unwrap()
                    .path()
                    .unwrap()
                    .ends_with("links/Note.md")
            );
        });

        write(&target, "target changed\n");
        target_sender.send_blocking(WatchBatch::default()).unwrap();
        cx.run_until_parked();
        assert_eq!(text_of(&view, cx, id), "target changed\n");

        write(&target, "target changed again\n");
        link_sender.send_blocking(WatchBatch::default()).unwrap();
        cx.run_until_parked();
        assert_eq!(text_of(&view, cx, id), "target changed again\n");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[gpui::test]
    fn a_symlink_target_in_the_same_directory_is_watched_by_name(cx: &mut gpui::TestAppContext) {
        inject_watch_error(None);
        let root = watch_test_root("link-same-dir");
        let target = root.join("Real.md");
        let link = root.join("Note.md");
        write(&target, "target\n");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let view = view_with_files(cx, &[&link]);
        let id = ids(&view, cx)[0];
        view.update(cx, |view, cx| view.sync_document_watches(cx));
        cx.run_until_parked();
        // One directory watch serves both names, however the platform spells
        // the directory (macOS: `/var` vs `/private/var`).
        let sender = only_batch_sender(&view, cx);

        write(&target, "target changed\n");
        sender.send_blocking(WatchBatch::default()).unwrap();
        cx.run_until_parked();
        assert_eq!(text_of(&view, cx, id), "target changed\n");

        view.update(cx, |view, cx| {
            let _ = view.sessions.close(id, "Untitled");
            view.sync_document_watches(cx);
            assert!(view.document_watch.parents.is_empty());
        });
        let _ = sender.send_blocking(WatchBatch::default());
        cx.run_until_parked();
        assert!(sender.is_closed());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[gpui::test]
    fn a_relinked_symlink_follows_the_new_target_and_releases_the_old_watch(
        cx: &mut gpui::TestAppContext,
    ) {
        inject_watch_error(None);
        let (view, root, old_target) = symlinked_view(cx, "link-retarget", "real", "Old.md");
        let link = root.join("links/Note.md");
        let new_target = root.join("moved/New.md");
        write(&new_target, "new target\n");
        let id = ids(&view, cx)[0];
        view.update(cx, |view, cx| view.sync_document_watches(cx));
        cx.run_until_parked();
        let old_parent = std::fs::canonicalize(&old_target)
            .unwrap()
            .parent()
            .unwrap()
            .to_path_buf();
        let new_parent = std::fs::canonicalize(&new_target)
            .unwrap()
            .parent()
            .unwrap()
            .to_path_buf();
        let (link_sender, old_sender) = view.read_with(cx, |view, _| {
            assert_eq!(view.document_watch.parents.len(), 2);
            let link_parent = view
                .document_watch
                .parents
                .keys()
                .find(|parent| **parent != old_parent)
                .expect("the link's own directory is watched")
                .clone();
            (
                view.document_watch.parents[&link_parent]
                    .test_batches
                    .clone()
                    .unwrap(),
                view.document_watch.parents[&old_parent]
                    .test_batches
                    .clone()
                    .expect("the old target's directory is watched"),
            )
        });

        // The link is pointed at another file; the link's directory reports it.
        std::fs::remove_file(&link).unwrap();
        std::os::unix::fs::symlink(&new_target, &link).unwrap();
        link_sender.send_blocking(WatchBatch::default()).unwrap();
        cx.run_until_parked();
        assert_eq!(text_of(&view, cx, id), "new target\n");
        view.read_with(cx, |view, _| {
            // The displayed path stays the link and nothing became dirty.
            let session = view.sessions.get(id).unwrap();
            assert!(session.path().unwrap().ends_with("links/Note.md"));
            assert!(!session.is_dirty());
        });

        // The next sync watches the new target's directory and drops the old.
        view.update(cx, |view, cx| view.sync_document_watches(cx));
        cx.run_until_parked();
        let new_sender = view.read_with(cx, |view, _| {
            assert_eq!(view.document_watch.parents.len(), 2);
            assert!(!view.document_watch.parents.contains_key(&old_parent));
            view.document_watch.parents[&new_parent]
                .test_batches
                .clone()
                .expect("the new target's directory is watched")
        });

        // A late notification from the old target changes nothing.
        write(&old_target, "stale old target\n");
        let _ = old_sender.send_blocking(WatchBatch::default());
        cx.run_until_parked();
        assert_eq!(text_of(&view, cx, id), "new target\n");
        assert!(old_sender.is_closed());

        // A change to the new target is followed.
        write(&new_target, "new target changed\n");
        new_sender.send_blocking(WatchBatch::default()).unwrap();
        cx.run_until_parked();
        assert_eq!(text_of(&view, cx, id), "new target changed\n");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[gpui::test]
    fn the_resolved_target_watch_is_released_with_the_document(cx: &mut gpui::TestAppContext) {
        inject_watch_error(None);
        let (view, root, _target) = symlinked_view(cx, "link-release", "real", "Real.md");
        let id = ids(&view, cx)[0];
        view.update(cx, |view, cx| view.sync_document_watches(cx));
        cx.run_until_parked();
        let senders: Vec<_> = view.read_with(cx, |view, _| {
            view.document_watch
                .parents
                .values()
                .map(|watch| watch.test_batches.clone().unwrap())
                .collect()
        });
        assert_eq!(senders.len(), 2);

        view.update(cx, |view, cx| {
            let _ = view.sessions.close(id, "Untitled");
            view.sync_document_watches(cx);
            assert!(view.document_watch.parents.is_empty());
            assert!(view.document_watch.tracked.is_empty());
        });
        for sender in &senders {
            let _ = sender.send_blocking(WatchBatch::default());
        }
        cx.run_until_parked();
        assert!(senders.iter().all(async_channel::Sender::is_closed));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[gpui::test]
    fn files_in_different_directories_get_their_own_watch_and_release_it_on_close(
        cx: &mut gpui::TestAppContext,
    ) {
        inject_watch_error(None);
        let root = watch_test_root("separate");
        let (a, b) = (root.join("one/A.md"), root.join("two/B.md"));
        write(&a, "a\n");
        write(&b, "b\n");
        let view = view_with_files(cx, &[&a, &b]);
        let ids = ids(&view, cx);
        view.update(cx, |view, cx| {
            view.sync_document_watches(cx);
            assert_eq!(view.document_watch.parents.len(), 2);
        });
        let senders: Vec<_> = view.read_with(cx, |view, _| {
            view.document_watch
                .parents
                .values()
                .map(|watch| watch.test_batches.clone().unwrap())
                .collect()
        });

        view.update(cx, |view, cx| {
            let _ = view.sessions.close(ids[1], "Untitled");
            view.sync_document_watches(cx);
            assert_eq!(view.document_watch.parents.len(), 1);
            let _ = view.sessions.close(ids[0], "Untitled");
            view.sync_document_watches(cx);
            assert!(view.document_watch.parents.is_empty());
            assert!(view.document_watch.tracked.is_empty());
        });
        // Notifications from the released watches end their tasks and start
        // nothing.
        for sender in &senders {
            let _ = sender.send_blocking(WatchBatch::default());
        }
        cx.run_until_parked();
        assert!(senders.iter().all(async_channel::Sender::is_closed));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[gpui::test]
    fn a_notification_checks_the_open_file_and_a_burst_converges_on_the_latest(
        cx: &mut gpui::TestAppContext,
    ) {
        inject_watch_error(None);
        let root = watch_test_root("burst");
        let a = root.join("A.md");
        let unopened = root.join("Unopened.md");
        write(&a, "a\n");
        write(&unopened, "never read\n");
        let view = view_with_files(cx, &[&a]);
        let id = ids(&view, cx)[0];
        view.update(cx, |view, cx| view.sync_document_watches(cx));
        cx.run_until_parked();
        let sender = only_batch_sender(&view, cx);

        for step in 1..=5 {
            write(&a, &format!("version {step}\n"));
            sender.send_blocking(WatchBatch::default()).unwrap();
        }
        cx.run_until_parked();

        assert_eq!(text_of(&view, cx, id), "version 5\n");
        view.read_with(cx, |view, _| {
            assert!(!view.sessions.get(id).unwrap().is_dirty());
        });
        let _ = std::fs::remove_dir_all(&root);
    }

    #[gpui::test]
    fn a_file_inside_the_watched_work_folder_is_not_registered_a_second_time(
        cx: &mut gpui::TestAppContext,
    ) {
        inject_watch_error(None);
        let root = watch_test_root("covered");
        let inside = root.join("folder/In.md");
        let outside = root.join("outside/Out.md");
        write(&inside, "in\n");
        write(&outside, "out\n");
        let work = root.join("folder");
        let view = view_with_files(cx, &[&inside, &outside]);
        let ids = ids(&view, cx);
        write(&inside, "in changed\n");
        view.update(cx, |view, cx| {
            view.start_folder_watch(&work, cx);
            view.sync_document_watches(cx);
            // Only the file outside the folder needs its own parent watch.
            assert_eq!(view.document_watch.parents.len(), 1);
            assert!(!view.active_document_watch_problem());
        });
        cx.run_until_parked();

        // The work folder watcher started cleanly, and the file in it was
        // still checked once.
        assert_eq!(text_of(&view, cx, ids[0]), "in changed\n");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[gpui::test]
    fn a_failed_work_folder_watch_falls_back_to_watching_the_parent(
        cx: &mut gpui::TestAppContext,
    ) {
        let root = watch_test_root("folder-down");
        let inside = root.join("In.md");
        write(&inside, "in\n");
        let view = view_with_files(cx, &[&inside]);
        inject_watch_error(None);
        inject_folder_attach_error(Some("folder denied"));
        view.update(cx, |view, cx| {
            view.start_folder_watch(&root, cx);
            view.sync_document_watches(cx);
            // Nothing covers the file, so its own directory is watched, and
            // the folder's failure stays visible.
            assert_eq!(view.document_watch.parents.len(), 1);
            assert!(!view.active_document_watch_problem());
            assert!(view.folder_watch_warning().is_some());
        });
        cx.run_until_parked();
        inject_folder_attach_error(None);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[gpui::test]
    fn an_unwatchable_directory_is_shown_per_document_and_recovers_without_a_notification(
        cx: &mut gpui::TestAppContext,
    ) {
        let root = watch_test_root("failed");
        let a = root.join("A.md");
        write(&a, "a\n");
        let view = view_with_files(cx, &[&a]);
        let id = ids(&view, cx)[0];

        inject_watch_error(Some("denied"));
        view.update(cx, |view, cx| {
            view.sync_document_watches(cx);
            view.sessions.open_untitled("", "Untitled");
        });
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            let warnings = view.footer_warnings();
            assert_eq!(warnings.len(), 1);
            assert!(warnings[0].contains("denied"));
            assert!(view.document_watch.retry_scheduled);
        });

        // Neither a later status message nor a tab switch hides it, and it
        // stays tied to the document that is not followed.
        view.update(cx, |view, _| {
            view.status = Some("Saved".to_owned());
            view.sessions.activate(id);
            assert!(view.active_document_watch_problem());
            let untitled = view
                .sessions
                .sessions()
                .map(DocumentSession::id)
                .find(|other| *other != id)
                .unwrap();
            view.sessions.activate(untitled);
            assert!(!view.active_document_watch_problem());
            assert_eq!(view.footer_warnings().len(), 1);
        });

        // Retrying while the cause persists reads no file and keeps one timer.
        write(&a, "a changed\n");
        cx.executor().advance_clock(DOCUMENT_WATCH_RETRY_DELAY);
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert_eq!(view.footer_warnings().len(), 1);
            assert!(view.document_watch.retry_scheduled);
        });
        assert_eq!(text_of(&view, cx, id), "a\n");

        // Once the cause is gone, the next retry registers the watch again and
        // checks the file, which changed while nothing was watching.
        inject_watch_error(None);
        cx.executor().advance_clock(DOCUMENT_WATCH_RETRY_DELAY);
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert!(view.footer_warnings().is_empty());
            assert!(!view.document_watch.retry_scheduled);
        });
        assert_eq!(text_of(&view, cx, id), "a changed\n");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[gpui::test]
    fn the_manual_recheck_registers_the_watch_again_and_reads_the_file(
        cx: &mut gpui::TestAppContext,
    ) {
        let root = watch_test_root("manual");
        let a = root.join("A.md");
        write(&a, "a\n");
        let view = view_with_files(cx, &[&a]);
        let id = ids(&view, cx)[0];
        inject_watch_error(Some("denied"));
        view.update(cx, |view, cx| view.sync_document_watches(cx));
        view.read_with(cx, |view, _| assert!(view.active_document_watch_problem()));

        inject_watch_error(None);
        write(&a, "a changed\n");
        view.update(cx, |view, cx| view.recheck_active_document(cx));
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert!(!view.active_document_watch_problem());
            assert!(view.footer_warnings().is_empty());
        });
        assert_eq!(text_of(&view, cx, id), "a changed\n");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[gpui::test]
    fn a_failure_report_registers_the_watch_again_and_checks_the_files(
        cx: &mut gpui::TestAppContext,
    ) {
        inject_watch_error(None);
        let root = watch_test_root("overflow");
        let a = root.join("A.md");
        write(&a, "a\n");
        let view = view_with_files(cx, &[&a]);
        let id = ids(&view, cx)[0];
        view.update(cx, |view, cx| view.sync_document_watches(cx));
        cx.run_until_parked();
        let (sender, token) = (only_batch_sender(&view, cx), only_token(&view, cx));

        write(&a, "a changed\n");
        sender
            .send_blocking(WatchBatch {
                failure: Some("notifications were dropped".to_owned()),
            })
            .unwrap();
        cx.run_until_parked();

        assert_eq!(text_of(&view, cx, id), "a changed\n");
        assert_ne!(only_token(&view, cx), token, "the watch was registered again");
        assert!(sender.is_closed(), "the old delivery task has ended");
        view.read_with(cx, |view, _| assert!(view.footer_warnings().is_empty()));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[gpui::test]
    fn a_failure_report_with_no_way_to_reconnect_is_not_a_success(cx: &mut gpui::TestAppContext) {
        inject_watch_error(None);
        let root = watch_test_root("lost");
        let a = root.join("A.md");
        write(&a, "a\n");
        let view = view_with_files(cx, &[&a]);
        view.update(cx, |view, cx| view.sync_document_watches(cx));
        cx.run_until_parked();
        let sender = only_batch_sender(&view, cx);

        inject_watch_error(Some("directory is gone"));
        sender
            .send_blocking(WatchBatch {
                failure: Some("the work folder was removed or renamed".to_owned()),
            })
            .unwrap();
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert!(view.active_document_watch_problem());
            assert_eq!(view.footer_warnings().len(), 1);
        });
        inject_watch_error(None);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[gpui::test]
    fn a_notification_from_a_closed_renamed_or_replaced_workspace_is_rejected(
        cx: &mut gpui::TestAppContext,
    ) {
        inject_watch_error(None);
        let root = watch_test_root("stale");
        let (a, renamed) = (root.join("one/A.md"), root.join("two/Renamed.md"));
        write(&a, "a\n");
        write(&renamed, "renamed\n");
        let view = view_with_files(cx, &[&a]);
        let id = ids(&view, cx)[0];
        view.update(cx, |view, cx| view.sync_document_watches(cx));
        cx.run_until_parked();

        // The document moves to another directory: the old directory's watch
        // is released and its late batch starts nothing.
        let old_sender = only_batch_sender(&view, cx);
        let loaded = OsFileService.load(&renamed).unwrap();
        view.update(cx, |view, cx| {
            view.sessions.get_mut(id).unwrap().adopt(loaded);
            view.sync_document_watches(cx);
        });
        cx.run_until_parked();
        write(&a, "late external write\n");
        old_sender.send_blocking(WatchBatch::default()).unwrap();
        cx.run_until_parked();
        assert!(old_sender.is_closed());
        assert_eq!(text_of(&view, cx, id), "renamed\n");

        // A work folder switch discards every registration of the old one.
        let before_switch = only_token(&view, cx);
        let switch_sender = only_batch_sender(&view, cx);
        view.update(cx, |view, cx| {
            view.work_folder_generation = view.work_folder_generation.wrapping_add(1);
            view.sync_document_watches(cx);
        });
        cx.run_until_parked();
        assert_ne!(only_token(&view, cx), before_switch);
        write(&renamed, "written after the switch\n");
        switch_sender.send_blocking(WatchBatch::default()).unwrap();
        cx.run_until_parked();
        assert!(switch_sender.is_closed());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[gpui::test]
    fn returning_to_the_app_checks_the_open_files_and_keeps_unsaved_edits(
        cx: &mut gpui::TestAppContext,
    ) {
        inject_watch_error(None);
        let root = watch_test_root("return");
        let (clean, edited) = (root.join("Clean.md"), root.join("Edited.md"));
        write(&clean, "clean\n");
        write(&edited, "edited\n");
        let view = view_with_files(cx, &[&clean, &edited]);
        let ids = ids(&view, cx);
        view.update(cx, |view, cx| view.sync_document_watches(cx));
        cx.run_until_parked();

        view.update(cx, |view, cx| {
            view.editor_mut().insert_text("typed ").unwrap();
            view.after_input(cx);
        });
        write(&clean, "clean, changed while away\n");
        write(&edited, "edited elsewhere\n");
        view.update(cx, |view, cx| view.recheck_documents_on_return(cx));
        cx.run_until_parked();

        assert_eq!(text_of(&view, cx, ids[0]), "clean, changed while away\n");
        view.read_with(cx, |view, _| {
            let session = view.sessions.get(ids[1]).unwrap();
            assert!(session.is_dirty());
            assert!(session.editor().document().full_text().starts_with("typed "));
            assert!(view.active_resolution_choices().is_some());
        });
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Finishes every registration held back by
    /// `HOLD_DOCUMENT_WATCH_REGISTRATION`, as the background thread would.
    fn release_held_registrations(view: &gpui::Entity<EditorView>, cx: &mut gpui::TestAppContext) {
        let held: Vec<HeldRegistration> =
            HELD_DOCUMENT_WATCH_REGISTRATIONS.with(|held| std::mem::take(&mut *held.borrow_mut()));
        for (parent, token, outcome) in held {
            view.update(cx, |view, cx| {
                view.complete_document_watch(&parent, token, outcome, cx);
            });
        }
        cx.run_until_parked();
    }

    fn hold_registrations(hold: bool) {
        HOLD_DOCUMENT_WATCH_REGISTRATION.with(|slot| slot.set(hold));
        if !hold {
            HELD_DOCUMENT_WATCH_REGISTRATIONS.with(|held| held.borrow_mut().clear());
        }
    }

    fn held_count() -> usize {
        HELD_DOCUMENT_WATCH_REGISTRATIONS.with(|held| held.borrow().len())
    }

    #[gpui::test]
    fn a_slow_registration_does_not_block_the_view_and_the_first_check_waits_for_it(
        cx: &mut gpui::TestAppContext,
    ) {
        inject_watch_error(None);
        hold_registrations(true);
        let root = watch_test_root("slow");
        let a = root.join("A.md");
        write(&a, "a\n");
        let view = view_with_files(cx, &[&a]);
        let id = ids(&view, cx)[0];
        write(&a, "a changed\n");

        // Several renders while the registration is outstanding: sync returns
        // at once, registers once, and checks nothing yet.
        view.update(cx, |view, cx| {
            view.sync_document_watches(cx);
            view.sync_document_watches(cx);
            view.sync_document_watches(cx);
            assert_eq!(view.document_watch.parents.len(), 1);
            assert!(view.document_watch.parents.values().all(|watch| watch.pending));
            assert!(!view.active_document_watch_problem());
            // The editor keeps working meanwhile.
            view.editor_mut().insert_text("typed ").unwrap();
            view.after_input(cx);
        });
        cx.run_until_parked();
        assert_eq!(held_count(), 1, "one registration, not one per render");
        view.read_with(cx, |view, _| {
            assert!(view.document_watch.tracked[&id].initial_pending);
            let session = view.sessions.get(id).unwrap();
            assert!(session.editor().document().full_text().starts_with("typed "));
        });

        release_held_registrations(&view, cx);
        hold_registrations(false);
        view.read_with(cx, |view, _| {
            assert!(view.document_watch.parents.values().all(|watch| !watch.pending));
            assert!(!view.document_watch.tracked[&id].initial_pending);
            assert!(!view.active_document_watch_problem());
            // The first check ran after the registration: the external change
            // is not written over the unsaved text, and the conflict is kept.
            let session = view.sessions.get(id).unwrap();
            assert!(session.is_dirty());
            assert!(session.editor().document().full_text().starts_with("typed "));
            assert!(view.active_resolution_choices().is_some());
        });
        only_batch_sender(&view, cx);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[gpui::test]
    fn a_registration_finished_after_close_move_or_folder_switch_is_discarded(
        cx: &mut gpui::TestAppContext,
    ) {
        inject_watch_error(None);
        hold_registrations(true);
        let root = watch_test_root("late");
        let (a, moved) = (root.join("one/A.md"), root.join("two/Moved.md"));
        write(&a, "a\n");
        write(&moved, "moved\n");
        let view = view_with_files(cx, &[&a]);
        let id = ids(&view, cx)[0];
        view.update(cx, |view, cx| view.sync_document_watches(cx));
        assert_eq!(held_count(), 1);

        // The document moves to another directory before the registration of
        // the old one finishes.
        let loaded = OsFileService.load(&moved).unwrap();
        view.update(cx, |view, cx| {
            view.sessions.get_mut(id).unwrap().adopt(loaded);
            view.sync_document_watches(cx);
        });
        assert_eq!(held_count(), 2);
        write(&a, "late external write\n");
        write(&moved, "moved changed\n");
        release_held_registrations(&view, cx);
        view.read_with(cx, |view, _| {
            let parent = watch_parent(&moved).unwrap();
            assert_eq!(view.document_watch.parents.len(), 1);
            assert!(!view.document_watch.parents[&parent].pending);
        });
        // Only the current registration checked the current document.
        assert_eq!(text_of(&view, cx, id), "moved changed\n");

        // Closing the document before the registration finishes: nothing is
        // kept, and a later sync registers afresh.
        view.update(cx, |view, cx| {
            let _ = view.sessions.close(id, "Untitled");
            view.work_folder_generation = view.work_folder_generation.wrapping_add(1);
            view.sync_document_watches(cx);
        });
        let reopened = OsFileService.load(&a).unwrap();
        view.update(cx, |view, cx| {
            view.sessions.apply_open(None, reopened);
            view.sync_document_watches(cx);
        });
        let first_token = only_token(&view, cx);
        view.update(cx, |view, cx| {
            view.work_folder_generation = view.work_folder_generation.wrapping_add(1);
            view.sync_document_watches(cx);
        });
        assert_ne!(only_token(&view, cx), first_token);
        release_held_registrations(&view, cx);
        view.read_with(cx, |view, _| {
            assert!(view.document_watch.parents.values().all(|watch| !watch.pending));
        });
        hold_registrations(false);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[gpui::test]
    fn a_registration_that_fails_late_is_shown_and_recovers(cx: &mut gpui::TestAppContext) {
        inject_watch_error(Some("slow and denied"));
        hold_registrations(true);
        let root = watch_test_root("late-fail");
        let a = root.join("A.md");
        write(&a, "a\n");
        let view = view_with_files(cx, &[&a]);
        let id = ids(&view, cx)[0];
        view.update(cx, |view, cx| view.sync_document_watches(cx));
        view.read_with(cx, |view, _| {
            // Still pending: neither healthy nor reported as failed.
            assert!(view.footer_warnings().is_empty());
            assert!(!view.document_watch.retry_scheduled);
        });

        release_held_registrations(&view, cx);
        view.read_with(cx, |view, _| {
            assert_eq!(view.footer_warnings().len(), 1);
            assert!(view.footer_warnings()[0].contains("slow and denied"));
            assert!(view.document_watch.retry_scheduled);
        });
        assert_eq!(text_of(&view, cx, id), "a\n", "a failed watch checks nothing");

        inject_watch_error(None);
        write(&a, "a changed\n");
        cx.executor().advance_clock(DOCUMENT_WATCH_RETRY_DELAY);
        cx.run_until_parked();
        release_held_registrations(&view, cx);
        hold_registrations(false);
        view.read_with(cx, |view, _| assert!(view.footer_warnings().is_empty()));
        assert_eq!(text_of(&view, cx, id), "a changed\n");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[gpui::test]
    fn the_activation_observer_is_registered_once_for_the_window(cx: &mut gpui::TestAppContext) {
        inject_watch_error(None);
        let root = watch_test_root("activation");
        let a = root.join("A.md");
        write(&a, "a\n");
        let loaded = OsFileService.load(&a).unwrap();
        let (view, cx) = cx.add_window_view(|_, cx| {
            EditorView::from_sessions(
                SessionSet::with_loaded(loaded),
                Arc::new(OsFileService),
                StateStores::memory(),
                cx,
            )
        });
        for _ in 0..3 {
            cx.update(|window, app| {
                view.update(app, |view, cx| view.observe_window_return(window, cx));
            });
        }
        view.read_with(cx, |view, _| {
            assert!(view.document_watch.activation.is_some());
        });
        let _ = std::fs::remove_dir_all(&root);
    }
}
