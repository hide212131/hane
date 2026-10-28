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
}

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
}

/// What the watch lifecycle knows about one open saved document.
struct TrackedDocument {
    path: PathBuf,
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

impl EditorView {
    /// Why the file of `id` is not being followed, or `None` when it is (or
    /// when it has no file). A document inside a healthy work folder is
    /// covered by the folder watch.
    pub(super) fn document_watch_problem(&self, id: SessionId) -> Option<String> {
        let path = self.sessions.get(id)?.path()?;
        if self.folder_watch_covers(path) {
            return None;
        }
        let parent = watch_parent(path)?;
        self.document_watch.parents.get(&parent)?.failure.clone()
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
        let documents: Vec<(SessionId, PathBuf)> = self
            .sessions
            .sessions()
            .filter_map(|session| Some((session.id(), session.path()?.to_path_buf())))
            .collect();
        self.document_watch
            .tracked
            .retain(|id, _| documents.iter().any(|(open, _)| open == id));
        for (id, path) in &documents {
            let current = self
                .document_watch
                .tracked
                .get(id)
                .is_some_and(|tracked| tracked.path == *path);
            if !current {
                self.document_watch.tracked.insert(
                    *id,
                    TrackedDocument {
                        path: path.clone(),
                        initial_pending: true,
                    },
                );
            }
        }

        let mut wanted: HashMap<PathBuf, Vec<OsString>> = HashMap::new();
        for (_, path) in &documents {
            if self.folder_watch_covers(path) {
                continue;
            }
            let (Some(parent), Some(name)) = (watch_parent(path), path.file_name()) else {
                continue;
            };
            wanted.entry(parent).or_default().push(name.to_os_string());
        }
        self.document_watch
            .parents
            .retain(|parent, _| wanted.contains_key(parent));
        let mut failed = false;
        for (parent, names) in &wanted {
            if let Some(existing) = self.document_watch.parents.get(parent) {
                existing.names.set(names);
                continue;
            }
            let set = WatchedNames::new(names);
            let watch = self.attach_document_watch(parent, &set, cx);
            failed |= watch.failure.is_some();
            self.document_watch.parents.insert(parent.clone(), watch);
        }
        if failed {
            self.schedule_document_watch_retry(cx);
        }

        // The first check comes after the watch is registered, so a change
        // between the two arrives as a notification instead of being lost.
        let pending: Vec<(SessionId, PathBuf)> = self
            .document_watch
            .tracked
            .iter()
            .filter(|(_, tracked)| tracked.initial_pending)
            .map(|(id, tracked)| (*id, tracked.path.clone()))
            .collect();
        for (id, path) in pending {
            let watched = self.folder_watch_covers(&path)
                || watch_parent(&path)
                    .and_then(|parent| self.document_watch.parents.get(&parent))
                    .is_some_and(|watch| watch.failure.is_none());
            if !watched {
                continue;
            }
            if let Some(tracked) = self.document_watch.tracked.get_mut(&id) {
                tracked.initial_pending = false;
            }
            self.start_external_check(id, cx);
        }
    }

    /// Registers a watch on `parent` and starts the task that delivers its
    /// notifications. A failure is returned inside the value, not raised: the
    /// caller records it so it is shown and retried.
    fn attach_document_watch(
        &mut self,
        parent: &Path,
        names: &WatchedNames,
        cx: &mut Context<Self>,
    ) -> ParentWatch {
        self.document_watch.next_token = self.document_watch.next_token.wrapping_add(1);
        let token = self.document_watch.next_token;
        let mut watch = ParentWatch {
            token,
            names: names.clone(),
            _watcher: None,
            #[cfg(test)]
            test_batches: None,
            failure: None,
        };
        #[cfg(test)]
        {
            if let Some(message) = INJECTED_DOCUMENT_WATCH_ERROR.with(|slot| slot.borrow().clone())
            {
                watch.failure = Some(message);
                return watch;
            }
        }
        let (sender, receiver) = async_channel::unbounded();
        // GPUI's deterministic test scheduler must not be woken by a real OS
        // thread, so unit tests do not start the OS watcher. They feed batches
        // through `test_batches`; the OS watcher itself is covered by the
        // `hane_session` tests.
        #[cfg(not(test))]
        {
            match FolderWatcher::start_directory(
                parent,
                names.clone(),
                WatchTiming::default(),
                move |batch| sender.send_blocking(batch).is_ok(),
            ) {
                Ok(watcher) => watch._watcher = Some(watcher),
                Err(error) => {
                    watch.failure = Some(error.to_string());
                    return watch;
                }
            }
        }
        #[cfg(test)]
        {
            watch.test_batches = Some(sender);
        }
        let parent = parent.to_path_buf();
        cx.spawn(async move |view, cx| {
            // Ends when the watch is released (the channel closes), the view
            // is gone, or this registration was replaced.
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
        watch
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
        let fresh = self.attach_document_watch(parent, &names, cx);
        let failed = fresh.failure.is_some();
        self.document_watch.parents.insert(parent.to_path_buf(), fresh);
        if failed {
            self.schedule_document_watch_retry(cx);
        }
        if !failed || check_when_failed {
            self.check_documents_in(parent, cx);
        }
        cx.notify();
    }

    /// Starts the background check of every open document in `parent`. Files
    /// that are not open are never read.
    fn check_documents_in(&mut self, parent: &Path, cx: &mut Context<Self>) {
        let ids: Vec<SessionId> = self
            .sessions
            .sessions()
            .filter(|session| {
                session.path().and_then(watch_parent).as_deref() == Some(parent)
            })
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
        let Some(path) = self.sessions.active().path().map(Path::to_path_buf) else {
            return;
        };
        let down = watch_parent(&path).filter(|parent| {
            self.document_watch
                .parents
                .get(parent)
                .is_some_and(|watch| watch.failure.is_some())
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
