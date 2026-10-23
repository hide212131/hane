//! Keeps the sidebar's work folder listing in step with changes made outside
//! the app.
//!
//! `hane_session::FolderWatcher` reports coalesced OS notifications from a
//! background thread; this module delivers them to the view and re-lists the
//! folder on the background executor (`OsWorkFolderScanner`, which lists
//! names and metadata and never opens a note). The result is accepted only if
//! it still belongs to the folder it was requested for, the same
//! `work_folder_generation` rule every other background result follows.
//! Nothing here touches sessions, so an open note, its unsaved edits, and its
//! undo history are never replaced by a listing update.

use super::*;
#[cfg(not(test))]
use hane_session::WatchTiming;
use hane_session::{FolderWatcher, WatchBatch};

/// The watch and re-list bookkeeping for the current work folder. Replaced
/// wholesale (dropping the old watcher) whenever a folder is opened.
#[derive(Default)]
pub(super) struct FolderWatchState {
    /// Held only for its lifetime: dropping it releases the OS watch.
    watcher: Option<FolderWatcher>,
    /// Test-only stand-in for the watcher thread's sender; dropping it closes
    /// the channel just as dropping the watcher does.
    #[cfg(test)]
    test_batches: Option<async_channel::Sender<WatchBatch>>,
    refresh_running: bool,
    /// A notification arrived while a re-list was running (or before the
    /// first scan finished); one more re-list follows once it is safe.
    refresh_pending: bool,
    /// The notification source's last reported trouble (overflow, watch
    /// error, root removed), consumed by the next re-list.
    failure: Option<String>,
    /// Why the folder is not being watched right now. Unlike the transient
    /// status text it lives as long as this state does (one work folder), so
    /// later status updates cannot make an inactive watch look healthy. Set
    /// when attaching fails and cleared when a reattach succeeds.
    unwatched: Option<String>,
    /// The folder the (failed) attach was for; a delayed retry connects only
    /// while this is still the root it was scheduled for.
    watch_root: Option<PathBuf>,
    /// A delayed reattach is already waiting, so failures do not stack timers.
    retry_scheduled: bool,
}

/// Orders every work folder listing request (first scan, watcher re-list, and
/// the explicit re-list of the content search) by the moment it was started.
/// The generation and root cannot tell two scans of the same folder apart, so
/// a result is applied only if no request started later has already been
/// applied; a slow older scan never overwrites a newer listing.
#[derive(Default)]
pub(super) struct ListingRequests {
    started: u64,
    applied: u64,
}

/// How long to wait before trying to attach a watch that failed. Retrying is
/// timer-driven and never depends on a new OS notification arriving.
const FOLDER_WATCH_RETRY_DELAY: std::time::Duration = std::time::Duration::from_secs(5);

#[cfg(test)]
thread_local! {
    /// Test-only error injection: the next attaches fail with this message.
    static INJECTED_ATTACH_ERROR: std::cell::RefCell<Option<String>> =
        const { std::cell::RefCell::new(None) };
}

impl EditorView {
    /// The warning shown for as long as the current work folder's changes are
    /// not being watched.
    pub(super) fn folder_watch_warning(&self) -> Option<String> {
        self.folder_watch.unwatched.as_ref().map(|error| {
            format!(
                "作業フォルダの外部変更を監視できていません（サイドバーは自動更新されません）: {error}"
            )
        })
    }

    /// Takes the next listing request number; call when a scan is started.
    pub(super) fn begin_listing_request(&mut self) -> u64 {
        self.listing_requests.started = self.listing_requests.started.wrapping_add(1);
        self.listing_requests.started
    }

    /// Whether the result of listing request `request` may be applied. A
    /// `true` answer records it, so every older request is rejected from now on.
    pub(super) fn accept_listing_result(&mut self, request: u64) -> bool {
        if request <= self.listing_requests.applied {
            return false;
        }
        self.listing_requests.applied = request;
        true
    }

    /// Starts watching `root` for the folder about to be scanned, replacing
    /// (and thereby releasing) the previous folder's watch. Called *before*
    /// the initial scan so a change made while that scan runs still arrives
    /// as a notification afterward.
    pub(super) fn start_folder_watch(&mut self, root: &Path, cx: &mut Context<Self>) {
        self.folder_watch = FolderWatchState::default();
        if let Err(error) = self.connect_folder_watch(root, cx) {
            self.status = Some(format!(
                "Work folder changes are not being watched: {error}"
            ));
        }
    }

    fn connect_folder_watch(&mut self, root: &Path, cx: &mut Context<Self>) -> std::io::Result<()> {
        let result = self.attach_folder_watch(root, cx);
        self.folder_watch.unwatched = result.as_ref().err().map(|error| error.to_string());
        self.folder_watch.watch_root = Some(root.to_path_buf());
        if result.is_err() {
            self.schedule_folder_watch_retry(root, cx);
        }
        result
    }

    /// Waits, then reattaches without any notification. The retry is dropped
    /// if the work folder was replaced or the window is gone meanwhile; a
    /// failed retry schedules the next one, so there is at most one timer.
    fn schedule_folder_watch_retry(&mut self, root: &Path, cx: &mut Context<Self>) {
        if self.folder_watch.retry_scheduled {
            return;
        }
        self.folder_watch.retry_scheduled = true;
        let generation = self.work_folder_generation;
        let root = root.to_path_buf();
        cx.spawn(async move |view, cx| {
            cx.background_executor()
                .timer(FOLDER_WATCH_RETRY_DELAY)
                .await;
            let _ = view.update(cx, |view, cx| {
                view.retry_folder_watch(generation, &root, cx);
            });
        })
        .detach();
    }

    fn retry_folder_watch(&mut self, generation: u64, root: &Path, cx: &mut Context<Self>) {
        if generation != self.work_folder_generation
            || self.folder_watch.watch_root.as_deref() != Some(root)
        {
            // Belongs to a replaced folder; its bookkeeping was reset.
            return;
        }
        self.folder_watch.retry_scheduled = false;
        if self.folder_watch.unwatched.is_none() {
            return;
        }
        if self.connect_folder_watch(root, cx).is_ok() {
            // Changes made while unwatched were never reported; list again.
            self.refresh_work_folder_listing(cx);
            cx.notify();
        } else {
            cx.notify();
        }
    }

    fn attach_folder_watch(&mut self, root: &Path, cx: &mut Context<Self>) -> std::io::Result<()> {
        // Released first so the previous watcher's task ends instead of two
        // watches reporting the same folder.
        self.folder_watch.watcher = None;
        #[cfg(test)]
        {
            self.folder_watch.test_batches = None;
            if let Some(message) = INJECTED_ATTACH_ERROR.with(|slot| slot.borrow().clone()) {
                return Err(std::io::Error::other(message));
            }
        }
        let (sender, receiver) = async_channel::unbounded();
        // GPUI's deterministic test scheduler must not be woken by a real OS
        // thread, so unit tests do not start the OS watcher. They feed batches
        // through `test_batches` from the test thread instead; the OS watcher
        // itself is covered by the `hane_session` tests.
        #[cfg(not(test))]
        {
            let watcher = FolderWatcher::start(root, WatchTiming::default(), move |batch| {
                sender.send_blocking(batch).is_ok()
            })?;
            self.folder_watch.watcher = Some(watcher);
        }
        #[cfg(test)]
        {
            let _ = root;
            self.folder_watch.test_batches = Some(sender);
        }
        let generation = self.work_folder_generation;
        cx.spawn(async move |view, cx| {
            // Ends when the watcher is dropped (the channel closes), the view
            // is gone, or the folder this watch belonged to was replaced.
            while let Ok(batch) = receiver.recv().await {
                let keep_going = view
                    .update(cx, |view, cx| {
                        view.folder_watch_batch_arrived(generation, batch, cx)
                    })
                    .unwrap_or(false);
                if !keep_going {
                    break;
                }
            }
        })
        .detach();
        Ok(())
    }

    /// Returns whether the watch this batch came from is still current.
    fn folder_watch_batch_arrived(
        &mut self,
        generation: u64,
        batch: WatchBatch,
        cx: &mut Context<Self>,
    ) -> bool {
        if generation != self.work_folder_generation {
            return false;
        }
        if let Some(failure) = batch.failure {
            self.folder_watch.failure = Some(failure);
        }
        self.refresh_work_folder_listing(cx);
        true
    }

    /// Re-lists the current work folder on the background executor. Runs at
    /// most one re-list at a time; notifications arriving meanwhile collapse
    /// into one follow-up.
    pub(super) fn refresh_work_folder_listing(&mut self, cx: &mut Context<Self>) {
        let Some(root) = self
            .work_folder
            .as_ref()
            .map(|folder| folder.root().to_path_buf())
        else {
            // The first scan has not installed a folder yet; it resumes this
            // once it does.
            self.folder_watch.refresh_pending = true;
            return;
        };
        if self.folder_watch.refresh_running {
            self.folder_watch.refresh_pending = true;
            return;
        }
        self.folder_watch.refresh_running = true;
        self.folder_watch.refresh_pending = false;
        let mut failure = self.folder_watch.failure.take();
        if let Some(reason) = failure.as_mut() {
            // The notification source reported trouble, so what it will say
            // next cannot be trusted to cover what it missed. Reattach
            // before listing, so a change after the list is still noticed.
            if let Err(error) = self.connect_folder_watch(&root, cx) {
                *reason = format!("{reason}; could not reattach the watch: {error}");
            }
        }
        let generation = self.work_folder_generation;
        let request = self.begin_listing_request();
        cx.spawn(async move |view, cx| {
            let scan_root = root.clone();
            let result = cx
                .background_executor()
                .spawn(async move { OsWorkFolderScanner.scan(&scan_root) })
                .await;
            let _ = view.update(cx, |view, cx| {
                view.finish_work_folder_refresh(generation, request, &root, failure, result, cx);
            });
        })
        .detach();
    }

    fn finish_work_folder_refresh(
        &mut self,
        generation: u64,
        request: u64,
        root: &Path,
        failure: Option<String>,
        result: std::io::Result<WorkFolder>,
        cx: &mut Context<Self>,
    ) {
        if generation != self.work_folder_generation
            || self
                .work_folder
                .as_ref()
                .is_none_or(|folder| folder.root() != root)
        {
            // Listed for a folder that has since been replaced: its
            // bookkeeping was reset with the switch, so leave it alone.
            return;
        }
        self.folder_watch.refresh_running = false;
        if !self.accept_listing_result(request) {
            // A scan started later (or the explicit re-list of the content
            // search) has already installed a newer listing; this older
            // result must not overwrite it. Keep the unreported trouble for
            // the next re-list.
            if self.folder_watch.failure.is_none() {
                self.folder_watch.failure = failure;
            }
            if self.folder_watch.refresh_pending {
                self.refresh_work_folder_listing(cx);
            }
            return;
        }
        match result {
            Ok(mut fresh) => {
                fresh.set_sort_order(self.settings.sidebar_sort);
                // State keyed by a folder path survives for folders that are
                // still there and is dropped for the ones that are not.
                self.expanded_folders
                    .retain(|path| fresh.children_at(path).is_some());
                if self
                    .selected_folder
                    .as_ref()
                    .is_some_and(|path| fresh.children_at(path).is_none())
                {
                    self.selected_folder = None;
                    if self.sidebar_focus == SidebarFocus::Folder {
                        self.sidebar_focus = SidebarFocus::ActiveSession;
                    }
                }
                self.work_folder = Some(fresh);
                if let Some(reason) = failure {
                    self.status = Some(format!("Work folder was rescanned: {reason}"));
                }
            }
            Err(error) => {
                // The last good list stays; it is not replaced by an empty or
                // partial one. The trouble is reported, and remembered so the
                // next notification reattaches the watch.
                self.status = Some(format!("Could not refresh the work folder: {error}"));
                self.folder_watch.failure =
                    Some(failure.unwrap_or_else(|| "the last refresh failed".to_owned()));
            }
        }
        if self.folder_watch.refresh_pending {
            self.refresh_work_folder_listing(cx);
        }
        cx.notify();
    }

    /// Resumes a re-list that was requested before the first scan finished.
    pub(super) fn resume_pending_folder_refresh(&mut self, cx: &mut Context<Self>) {
        if self.folder_watch.refresh_pending {
            self.refresh_work_folder_listing(cx);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn watch_test_root(label: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQUENCE: AtomicU64 = AtomicU64::new(0);
        std::env::temp_dir().join(format!(
            "hane-folder-watch-{label}-{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ))
    }

    fn view_with_folder(
        cx: &mut gpui::TestAppContext,
        root: &Path,
    ) -> gpui::Entity<EditorView> {
        let work_folder = OsWorkFolderScanner.scan(root).unwrap();
        let view = gpui::AppContext::new(cx, |cx| {
            EditorView::from_sessions(
                SessionSet::with_untitled("", "Untitled"),
                Arc::new(OsFileService),
                StateStores::memory(),
                cx,
            )
        });
        view.update(cx, |view, _| view.work_folder = Some(work_folder));
        view
    }

    fn file_names(view: &EditorView) -> Vec<String> {
        let mut names: Vec<String> = view
            .work_folder
            .as_ref()
            .unwrap()
            .entries()
            .iter()
            .map(|entry| entry.file_name().to_owned())
            .collect();
        names.sort_unstable();
        names
    }

    #[gpui::test]
    fn a_refresh_installs_the_new_listing_and_prunes_state_of_removed_folders(
        cx: &mut gpui::TestAppContext,
    ) {
        let root = watch_test_root("prune");
        std::fs::create_dir_all(root.join("keep")).unwrap();
        std::fs::create_dir_all(root.join("gone")).unwrap();
        std::fs::write(root.join("keep/A.md"), "# A\n").unwrap();
        let view = view_with_folder(cx, &root);
        view.update(cx, |view, _| {
            view.expanded_folders.insert(root.join("keep"));
            view.expanded_folders.insert(root.join("gone"));
            view.selected_folder = Some(root.join("gone"));
            view.sidebar_focus = SidebarFocus::Folder;
        });

        std::fs::remove_dir_all(root.join("gone")).unwrap();
        std::fs::write(root.join("keep/B.md"), "# B\n").unwrap();
        view.update(cx, |view, cx| {
            let generation = view.work_folder_generation;
            let request = view.begin_listing_request();
            let fresh = OsWorkFolderScanner.scan(&root);
            view.finish_work_folder_refresh(generation, request, &root, None, fresh, cx);
        });

        view.read_with(cx, |view, _| {
            assert_eq!(file_names(view), ["A.md", "B.md"]);
            assert!(view.expanded_folders.contains(&root.join("keep")));
            assert!(!view.expanded_folders.contains(&root.join("gone")));
            assert_eq!(view.selected_folder, None);
            assert_eq!(view.sidebar_focus, SidebarFocus::ActiveSession);
        });
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[gpui::test]
    fn a_refresh_leaves_open_documents_and_unsaved_edits_alone(cx: &mut gpui::TestAppContext) {
        let root = watch_test_root("document");
        std::fs::create_dir_all(&root).unwrap();
        let view = view_with_folder(cx, &root);
        view.update(cx, |view, cx| {
            view.editor_mut().insert_text("unsaved text").unwrap();
            view.after_input(cx);
        });

        std::fs::write(root.join("External.md"), "# External\n").unwrap();
        view.update(cx, |view, cx| {
            let generation = view.work_folder_generation;
            let request = view.begin_listing_request();
            let fresh = OsWorkFolderScanner.scan(&root);
            view.finish_work_folder_refresh(generation, request, &root, None, fresh, cx);
        });

        view.read_with(cx, |view, _| {
            assert_eq!(file_names(view), ["External.md"]);
            assert_eq!(view.editor().document().full_text(), "unsaved text");
            assert!(view.active_session().is_dirty());
        });
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[gpui::test]
    fn a_refresh_result_for_a_replaced_folder_is_rejected(cx: &mut gpui::TestAppContext) {
        let root = watch_test_root("stale");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("Current.md"), "# Current\n").unwrap();
        let view = view_with_folder(cx, &root);

        let other = watch_test_root("stale-other");
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(other.join("Other.md"), "# Other\n").unwrap();
        view.update(cx, |view, cx| {
            let stale_generation = view.work_folder_generation;
            view.work_folder_generation = view.work_folder_generation.wrapping_add(1);
            // A result listed for the older generation.
            let request = view.begin_listing_request();
            let fresh = OsWorkFolderScanner.scan(&other);
            view.finish_work_folder_refresh(stale_generation, request, &other, None, fresh, cx);
            // And one for a root that is not the current folder.
            let generation = view.work_folder_generation;
            let request = view.begin_listing_request();
            let fresh = OsWorkFolderScanner.scan(&other);
            view.finish_work_folder_refresh(generation, request, &other, None, fresh, cx);
        });

        view.read_with(cx, |view, _| {
            assert_eq!(file_names(view), ["Current.md"]);
        });
        std::fs::remove_dir_all(&root).unwrap();
        std::fs::remove_dir_all(&other).unwrap();
    }

    #[gpui::test]
    fn an_older_scan_of_the_same_folder_finishing_last_is_rejected(
        cx: &mut gpui::TestAppContext,
    ) {
        let root = watch_test_root("reordered");
        std::fs::create_dir_all(&root).unwrap();
        let view = view_with_folder(cx, &root);

        view.update(cx, |view, cx| {
            let generation = view.work_folder_generation;
            // The older scan started first but saw the folder before the
            // external file appeared.
            let older_request = view.begin_listing_request();
            let older = OsWorkFolderScanner.scan(&root);
            std::fs::write(root.join("External.md"), "# External\n").unwrap();
            let newer_request = view.begin_listing_request();
            let newer = OsWorkFolderScanner.scan(&root);

            // The newer scan completes first, then the older one.
            view.finish_work_folder_refresh(generation, newer_request, &root, None, newer, cx);
            view.finish_work_folder_refresh(generation, older_request, &root, None, older, cx);
        });

        view.read_with(cx, |view, _| {
            assert_eq!(file_names(view), ["External.md"]);
        });
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[gpui::test]
    fn a_failed_refresh_keeps_the_last_listing_and_reports_the_error(
        cx: &mut gpui::TestAppContext,
    ) {
        let root = watch_test_root("failed");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("Kept.md"), "# Kept\n").unwrap();
        let view = view_with_folder(cx, &root);

        view.update(cx, |view, cx| {
            let generation = view.work_folder_generation;
            let request = view.begin_listing_request();
            let error = Err(std::io::Error::other("boom"));
            view.finish_work_folder_refresh(
                generation,
                request,
                &root,
                Some("watch error".to_owned()),
                error,
                cx,
            );
        });

        view.read_with(cx, |view, _| {
            assert_eq!(file_names(view), ["Kept.md"]);
            assert!(
                view.status
                    .as_deref()
                    .is_some_and(|status| status.contains("boom"))
            );
            assert_eq!(view.folder_watch.failure.as_deref(), Some("watch error"));
        });
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[gpui::test]
    fn a_notification_from_a_replaced_folder_does_not_start_a_refresh(
        cx: &mut gpui::TestAppContext,
    ) {
        let root = watch_test_root("old-notification");
        std::fs::create_dir_all(&root).unwrap();
        let view = view_with_folder(cx, &root);

        view.update(cx, |view, cx| {
            let stale_generation = view.work_folder_generation;
            view.work_folder_generation = view.work_folder_generation.wrapping_add(1);
            let keep = view.folder_watch_batch_arrived(stale_generation, WatchBatch::default(), cx);
            assert!(!keep, "the watch of a replaced folder is told to stop");
            assert!(!view.folder_watch.refresh_running);
        });
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[gpui::test]
    fn a_delivered_batch_relists_the_folder_and_a_replaced_watch_is_released(
        cx: &mut gpui::TestAppContext,
    ) {
        let root = watch_test_root("delivery");
        std::fs::create_dir_all(&root).unwrap();
        let view = view_with_folder(cx, &root);
        view.update(cx, |view, cx| {
            view.connect_folder_watch(&root, cx).unwrap();
        });

        std::fs::write(root.join("Added.md"), "# Added\n").unwrap();
        let sender = view.read_with(cx, |view, _| view.folder_watch.test_batches.clone().unwrap());
        sender.send_blocking(WatchBatch::default()).unwrap();
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert_eq!(file_names(view), ["Added.md"]);
            assert!(!view.folder_watch.refresh_running);
        });

        // Replacing the folder generation ends the delivery task, and a batch
        // sent by the old watch no longer starts a re-list.
        view.update(cx, |view, _| {
            view.work_folder_generation = view.work_folder_generation.wrapping_add(1);
        });
        std::fs::write(root.join("Late.md"), "# Late\n").unwrap();
        let _ = sender.send_blocking(WatchBatch::default());
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert_eq!(file_names(view), ["Added.md"]);
            assert!(!view.folder_watch.refresh_running);
        });
        assert!(sender.is_closed(), "the old delivery task has ended");
        std::fs::remove_dir_all(&root).unwrap();
    }

    fn inject_attach_error(message: Option<&str>) {
        INJECTED_ATTACH_ERROR.with(|slot| *slot.borrow_mut() = message.map(str::to_owned));
    }

    #[gpui::test]
    fn a_failed_watch_start_stays_visible_after_later_status_updates(
        cx: &mut gpui::TestAppContext,
    ) {
        let root = watch_test_root("start-failed");
        std::fs::create_dir_all(&root).unwrap();
        let view = view_with_folder(cx, &root);

        inject_attach_error(Some("no watch for you"));
        view.update(cx, |view, cx| {
            let text = view.editor().document().full_text();
            view.start_folder_watch(&root, cx);
            // The initial scan and the first note open overwrite the shared
            // status line, and a later operation does too.
            view.status = Some("Opening…".to_owned());
            view.status = Some("Opened".to_owned());
            view.status = Some("Saved".to_owned());

            let warnings = view.footer_warnings();
            assert_eq!(warnings.len(), 1);
            assert!(warnings[0].contains("no watch for you"));
            assert_eq!(view.editor().document().full_text(), text);
            assert!(!view.active_session().is_dirty());
        });

        // Reattaching while the failure continues keeps the warning.
        view.update(cx, |view, cx| {
            assert!(view.connect_folder_watch(&root, cx).is_err());
            assert_eq!(view.footer_warnings().len(), 1);
        });

        // A successful reattach removes it.
        inject_attach_error(None);
        view.update(cx, |view, cx| {
            view.connect_folder_watch(&root, cx).unwrap();
            assert!(view.footer_warnings().is_empty());
        });
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[gpui::test]
    fn a_failed_attach_is_retried_after_a_delay_without_a_notification(
        cx: &mut gpui::TestAppContext,
    ) {
        let root = watch_test_root("retry");
        std::fs::create_dir_all(&root).unwrap();
        let view = view_with_folder(cx, &root);

        inject_attach_error(Some("busy"));
        view.update(cx, |view, cx| view.start_folder_watch(&root, cx));
        cx.run_until_parked();
        view.read_with(cx, |view, _| assert_eq!(view.footer_warnings().len(), 1));

        // The scan succeeds, and the warning stays while nothing is watched.
        view.update(cx, |view, cx| {
            let generation = view.work_folder_generation;
            let request = view.begin_listing_request();
            let fresh = OsWorkFolderScanner.scan(&root);
            view.finish_work_folder_refresh(generation, request, &root, None, fresh, cx);
            assert_eq!(view.footer_warnings().len(), 1);
        });

        // A retry that fails again keeps the warning and waits for the next.
        cx.executor().advance_clock(FOLDER_WATCH_RETRY_DELAY);
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert_eq!(view.footer_warnings().len(), 1);
            assert!(view.folder_watch.test_batches.is_none());
            assert!(view.folder_watch.retry_scheduled);
        });

        // Once the cause is gone, the next delayed retry reconnects and
        // lists the folder again, with no new OS notification.
        inject_attach_error(None);
        std::fs::write(root.join("Missed.md"), "# Missed\n").unwrap();
        cx.executor().advance_clock(FOLDER_WATCH_RETRY_DELAY);
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert!(view.footer_warnings().is_empty());
            assert!(view.folder_watch.test_batches.is_some());
            assert!(!view.folder_watch.retry_scheduled);
            assert_eq!(file_names(view), ["Missed.md"]);
        });
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[gpui::test]
    fn a_retry_scheduled_for_a_replaced_folder_does_not_attach(cx: &mut gpui::TestAppContext) {
        let root = watch_test_root("retry-stale");
        std::fs::create_dir_all(&root).unwrap();
        let view = view_with_folder(cx, &root);

        inject_attach_error(Some("busy"));
        view.update(cx, |view, cx| view.start_folder_watch(&root, cx));
        inject_attach_error(None);
        view.update(cx, |view, _| {
            view.work_folder_generation = view.work_folder_generation.wrapping_add(1);
        });
        cx.executor().advance_clock(FOLDER_WATCH_RETRY_DELAY);
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert!(view.folder_watch.test_batches.is_none());
            assert_eq!(view.footer_warnings().len(), 1);
        });
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[gpui::test]
    fn switching_folders_drops_the_previous_folders_watch_warning(
        cx: &mut gpui::TestAppContext,
    ) {
        let root = watch_test_root("switch-warning");
        std::fs::create_dir_all(&root).unwrap();
        let view = view_with_folder(cx, &root);

        inject_attach_error(Some("denied"));
        view.update(cx, |view, cx| {
            view.start_folder_watch(&root, cx);
            assert_eq!(view.footer_warnings().len(), 1);
        });

        inject_attach_error(None);
        view.update(cx, |view, cx| {
            view.start_folder_watch(&root, cx);
            assert!(view.footer_warnings().is_empty());
        });
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[gpui::test]
    fn a_healthy_watch_shows_no_warning(cx: &mut gpui::TestAppContext) {
        let root = watch_test_root("healthy");
        std::fs::create_dir_all(&root).unwrap();
        let view = view_with_folder(cx, &root);
        inject_attach_error(None);
        view.update(cx, |view, cx| {
            view.start_folder_watch(&root, cx);
            assert!(view.folder_watch_warning().is_none());
            assert!(view.footer_warnings().is_empty());
        });
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[gpui::test]
    fn notifications_during_a_refresh_collapse_into_one_follow_up(cx: &mut gpui::TestAppContext) {
        let root = watch_test_root("collapse");
        std::fs::create_dir_all(&root).unwrap();
        let view = view_with_folder(cx, &root);

        view.update(cx, |view, cx| {
            view.folder_watch.refresh_running = true;
            for _ in 0..5 {
                let generation = view.work_folder_generation;
                assert!(view.folder_watch_batch_arrived(generation, WatchBatch::default(), cx));
            }
            assert!(view.folder_watch.refresh_pending);
            assert!(view.folder_watch.refresh_running);
        });
        std::fs::remove_dir_all(&root).unwrap();
    }
}
