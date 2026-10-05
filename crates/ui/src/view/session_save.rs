use super::*;

impl EditorView {
    /// Arms the debounce timer for the active session. Each call invalidates the
    /// timer armed by the previous keystroke, so a burst of typing produces one
    /// write at the end rather than one per key.
    pub(super) fn schedule_autosave(&mut self, cx: &mut Context<Self>) {
        let autosave = self.settings.autosave;
        let session = self.sessions.active_mut();
        session.note_edit();
        let Some(ticket) = session.autosave_ticket(autosave) else {
            return;
        };
        let id = session.id();
        cx.spawn(async move |view, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(750))
                .await;
            let should_save = view
                .read_with(cx, |view, _| {
                    view.sessions.active_id() == id
                        && view
                            .sessions
                            .active()
                            .autosave_is_current(ticket, view.settings.autosave)
                })
                .unwrap_or(false);
            if should_save {
                let _ = view.update(cx, |view, cx| view.save_current(cx));
            }
        })
        .detach();
    }

    /// Journals the active session's text into the recovery drafts on the
    /// same debounce cadence as autosave: a no-op unless the active session
    /// is an unnamed note in the current work folder. Kept separate from
    /// `schedule_autosave` because an unnamed note has no path to write to
    /// yet and must not wait for one to earn crash safety.
    ///
    /// The scheduled save targets the session it was armed for by id, not
    /// whichever session is active when the timer fires: switching to
    /// another note within the debounce window must not cancel the write, or
    /// edits made just before switching away are lost on a crash until the
    /// draft is revisited and edited again.
    pub(super) fn schedule_draft_save(&mut self, cx: &mut Context<Self>) {
        let id = self.sessions.active_id();
        let Some(draft_id) = self.work_folder_drafts.get(&id).map(|draft| draft.draft_id) else {
            return;
        };
        let Some(root) = self
            .work_folder
            .as_ref()
            .map(|folder| folder.root().to_path_buf())
        else {
            return;
        };
        let revision = self.sessions.active().revision();
        let draft_store = self.draft_store.clone();
        cx.spawn(async move |view, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(750))
                .await;
            let text = view
                .read_with(cx, |view, _| {
                    let session = view.sessions.get(id)?;
                    let current =
                        session.revision() == revision && view.work_folder_drafts.contains_key(&id);
                    current.then(|| session.editor().document().full_text())
                })
                .ok()
                .flatten();
            if let Some(text) = text {
                let _ = cx
                    .background_executor()
                    .spawn(async move { draft_store.write(&root, draft_id, &text) })
                    .await;
            }
        })
        .detach();
    }

    /// Writes every pending unnamed-note draft synchronously, bypassing the
    /// debounce in `schedule_draft_save`. Called from the app-quit hook
    /// registered in `from_sessions` and from `switch_to_work_folder`, and
    /// public so `main.rs` can also call it from a window-close hook: a
    /// normal quit — or closing the window, which on some platforms does not
    /// raise an app-quit event at all — gives a `schedule_draft_save` timer
    /// no chance to fire if it was armed less than 750ms earlier, so without
    /// this an unnamed note's last few keystrokes would only survive a crash
    /// (recovered from whatever the debounce last wrote), not a clean exit.
    pub fn flush_pending_drafts(&self) {
        if self.work_folder_drafts.is_empty() {
            return;
        }
        let Some(root) = self.work_folder.as_ref().map(WorkFolder::root) else {
            return;
        };
        for (&id, draft) in &self.work_folder_drafts {
            let Some(session) = self.sessions.get(id) else {
                continue;
            };
            let text = session.editor().document().full_text();
            let _ = self.draft_store.write(root, draft.draft_id, &text);
        }
    }

    /// Issue #6: arms the debounce timer that keeps a work-folder note's
    /// filename following its first H1. A no-op outside a work folder.
    pub(super) fn schedule_title_sync(&mut self, cx: &mut Context<Self>) {
        if self.work_folder.is_none() {
            return;
        }
        let id = self.sessions.active_id();
        let generation = self.sessions.active().generation();
        let revision = self.sessions.active().revision();
        self.title_sync_scheduled.insert(id, revision);
        cx.spawn(async move |view, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(750))
                .await;
            let _ = view.update(cx, |view, cx| {
                if view.title_sync_scheduled.get(&id).copied() == Some(revision) {
                    view.title_sync_scheduled.remove(&id);
                    view.run_title_sync(id, generation, revision, cx);
                }
            });
        })
        .detach();
    }

    /// Decides, for the session the timer was armed for, whether its H1
    /// should create, rename, or stop auto-managing its filename — recomputed
    /// fresh against the document as it stands now, since edits may have
    /// landed while the timer was pending.
    fn run_title_sync(
        &mut self,
        id: SessionId,
        generation: u64,
        revision: Revision,
        cx: &mut Context<Self>,
    ) {
        if self
            .inline_rename
            .as_ref()
            .is_some_and(|rename| rename.pending)
        {
            self.title_sync_deferred.insert(id);
            return;
        }
        if self.title_sync_in_flight.contains(&id) {
            // Another probe or write for this session is already running; the
            // next edit re-arms this timer, so nothing is lost by skipping.
            return;
        }
        let Some(work_folder_root) = self
            .work_folder
            .as_ref()
            .map(|folder| folder.root().to_path_buf())
        else {
            return;
        };
        let Some(session) = self.sessions.get(id) else {
            return;
        };
        if session.generation() != generation || session.revision() != revision {
            return; // superseded by a later keystroke or a document replacement
        }
        let text = session.editor().document().full_text();
        let extracted = extract_h1_title(&text);
        let current_stem = session
            .path()
            .and_then(Path::file_stem)
            .and_then(|stem| stem.to_str());
        let action = decide_title_sync(session.auto_title(), current_stem, extracted.as_deref());
        match action {
            TitleSyncAction::None => {}
            TitleSyncAction::StopTracking => {
                if let Some(session) = self.sessions.get_mut(id) {
                    session.stop_auto_naming();
                }
            }
            TitleSyncAction::CreateNamed(title) => {
                // The folder this note was started in, if it was started
                // from a selected sidebar folder; otherwise the work folder
                // root, the same as before folders existed.
                let target_directory = self
                    .work_folder_drafts
                    .get(&id)
                    .map(|draft| draft.target_directory.clone())
                    .unwrap_or(work_folder_root);
                self.begin_title_create(id, target_directory, title, cx);
            }
            TitleSyncAction::Rename(title) => self.begin_title_rename(id, title, cx),
        }
    }

    /// Picks a collision-free `<title>.md` under `target_directory` in the
    /// background, then writes the still-untitled session's content there
    /// for the first time through the same save machinery as any other
    /// write.
    fn begin_title_create(
        &mut self,
        id: SessionId,
        target_directory: PathBuf,
        title: String,
        cx: &mut Context<Self>,
    ) {
        let root = target_directory;
        self.title_sync_in_flight.insert(id);
        let files = self.files.clone();
        let probe_title = title.clone();
        cx.spawn(async move |view, cx| {
            let probe_files = files.clone();
            let probe_root = root.clone();
            let candidate = cx
                .background_executor()
                .spawn(async move {
                    unique_markdown_filename(&probe_title, |name| {
                        probe_files.stamp(&probe_root.join(name)).is_some()
                    })
                })
                .await;
            let _ = view.update(cx, |view, cx| {
                view.title_sync_in_flight.remove(&id);
                // A session that no longer exists (closed, or replaced while
                // the probe was running) defers the same as one that reports
                // `should_defer_h1_create`; `retry_title_sync` picks this
                // back up once whatever holds the slot finishes.
                let should_skip = view
                    .sessions
                    .get(id)
                    .is_none_or(DocumentSession::should_defer_h1_create);
                if should_skip {
                    return;
                }
                view.title_sync_pending.insert(id, title.clone());
                view.save_session(id, SaveIntent::CreateNew(root.join(&candidate)), cx);
            });
        })
        .detach();
    }

    /// Picks a collision-free `<title>.md` in the note's own directory in the
    /// background, then renames the session's current file to it. The
    /// directory is the file's current parent, not the work folder root, so
    /// an H1-driven rename never moves a note out of the folder it lives in.
    /// `DocumentSession`'s own `apply_file_event` is what actually moves the
    /// session, the same path a filer-originated rename would take, so a
    /// rename that lands after the file already moved on for some other
    /// reason is safely ignored.
    fn begin_title_rename(&mut self, id: SessionId, title: String, cx: &mut Context<Self>) {
        let Some(session) = self.sessions.get_mut(id) else {
            return;
        };
        let Some(from) = session.path().map(Path::to_path_buf) else {
            return;
        };
        let Some(root) = from.parent().map(Path::to_path_buf) else {
            return;
        };
        // Reserved synchronously, before any `await`: a concurrent autosave
        // that fires in the same tick must see the slot already taken and
        // queue behind it, not race the rename to the filesystem. A `None`
        // here means a write is already in flight; the rename is skipped for
        // now rather than racing that write instead, and the next debounce
        // (armed by any further edit) tries again.
        let Some(ticket) = session.begin_rename() else {
            return;
        };
        // Captured now, not re-read once the rename lands: a work-folder
        // switch installs a fresh `SessionSet` whose ids restart at the
        // same values the old one used, and `DocumentSession::adopt` bumps
        // the generation without touching the workspace at all, so either
        // kind of document exchange can leave a stale completion naming the
        // same id as a document that was never the one this rename was
        // for. Binding the full instance here, before the completion ever
        // reaches `finish_title_rename`, is what tells them apart.
        let instance = DocumentInstance {
            generation: session.generation(),
            workspace: self.work_folder_generation,
        };
        self.title_sync_in_flight.insert(id);
        let files = self.files.clone();
        let probe_title = title.clone();
        let probe_from = from.clone();
        cx.spawn(async move |view, cx| {
            let probe_files = files.clone();
            let probe_root = root.clone();
            let rename_from = from.clone();
            let (target, rename_result) = cx
                .background_executor()
                .spawn(async move {
                    let candidate = unique_markdown_filename(&probe_title, |name| {
                        let candidate = probe_root.join(name);
                        candidate != probe_from && probe_files.stamp(&candidate).is_some()
                    });
                    let target = probe_root.join(candidate);
                    let result = probe_files.rename(&rename_from, &target);
                    (target, result)
                })
                .await;
            let _ = view.update(cx, |view, cx| {
                if view.document_instance(id) != Some(instance) {
                    // The document this rename was for is gone, or this id
                    // now names a different instance of it — a work-folder
                    // switch, or an in-place `adopt` that replaced the
                    // document without touching the workspace. The rename
                    // itself already ran against the old path on disk and
                    // cannot be undone from here, but no bookkeeping for
                    // whatever document now holds `id` may be touched: not
                    // its in-flight marker, not its queued save, not its
                    // pending-close request.
                    return;
                }
                view.title_sync_in_flight.remove(&id);
                let attempt = TitleRenameAttempt {
                    id,
                    ticket,
                    from,
                    target,
                    title,
                };
                view.finish_title_rename(attempt, rename_result, cx);
            });
        })
        .detach();
    }

    fn finish_title_rename(
        &mut self,
        attempt: TitleRenameAttempt,
        result: std::io::Result<()>,
        cx: &mut Context<Self>,
    ) {
        let TitleRenameAttempt {
            id,
            ticket,
            from,
            target,
            title,
        } = attempt;
        if let Ok(()) = result {
            let outcomes = self.sessions.apply_file_event(&FileEvent::Renamed {
                from: from.clone(),
                to: target.clone(),
            });
            let applied = outcomes.iter().any(|(session_id, outcome)| {
                *session_id == id && *outcome == FileEventOutcome::Renamed
            });
            if applied {
                if let Some(session) = self.sessions.get_mut(id) {
                    session.note_auto_named(title);
                }
                if let Some(folder) = self.work_folder.as_mut() {
                    folder.rename(&from, &target);
                }
                self.recent.rename(&from, &target);
                if let Err(error) = self.stores.recent_files().store(&self.recent) {
                    self.status = Some(format!("Recent files failed: {error}"));
                }
            }
        }
        // The picked name may have been raced away, or the file may have
        // moved on for some other reason; either way the save slot the
        // rename reserved must be released so anything it queued behind
        // itself (an autosave that arrived in the meantime) now runs against
        // whichever path the session actually ended up at.
        if let Some(session) = self.sessions.get_mut(id) {
            session.finish_rename(ticket);
            if let Some(pending) = session.take_pending_save() {
                self.save_session(id, pending, cx);
            }
        }
        self.resolve_pending_tab_close(id, cx);
        cx.notify();
    }

    /// A note stops being a draft once it earns a real path, whether through
    /// the (future) H1-derived rename or a manual Save As. The recovery
    /// journal entry is removed on a background thread; a failure here just
    /// leaves a harmless leftover file, never lost content.
    fn retire_work_folder_draft(&mut self, id: SessionId, cx: &mut Context<Self>) {
        let Some(draft) = self.work_folder_drafts.remove(&id) else {
            return;
        };
        let draft_id = draft.draft_id;
        let Some(root) = self
            .work_folder
            .as_ref()
            .map(|folder| folder.root().to_path_buf())
        else {
            return;
        };
        let draft_store = self.draft_store.clone();
        cx.background_executor()
            .spawn(async move {
                let _ = draft_store.remove(&root, draft_id);
            })
            .detach();
    }

    /// Marks a session auto-managed once its H1-derived first write has
    /// actually landed, not merely been requested, and adds the new note to
    /// the work folder index so it appears in the sidebar without waiting
    /// for the next full rescan.
    fn apply_pending_title_sync(&mut self, id: SessionId, path: &Path) {
        let Some(title) = self.title_sync_pending.remove(&id) else {
            return;
        };
        if let Some(session) = self.sessions.get_mut(id) {
            session.note_auto_named(title);
        }
        if let Some(folder) = self.work_folder.as_mut() {
            folder.insert(path.to_path_buf());
        }
    }

    /// Re-decides title sync for a session right after one of its writes
    /// lands. `begin_title_rename` defers instead of racing a save that is
    /// still in flight (see `DocumentSession::begin_rename`), so the rename
    /// it deferred needs a prompt to try again once that save is done,
    /// rather than waiting on the next keystroke to re-arm the debounce —
    /// which may never come, if the H1 edit that wanted the rename was the
    /// document's last edit before the autosave it lost the race to.
    fn retry_title_sync(&mut self, id: SessionId, cx: &mut Context<Self>) {
        let Some(session) = self.sessions.get(id) else {
            return;
        };
        let generation = session.generation();
        let revision = session.revision();
        self.run_title_sync(id, generation, revision, cx);
    }

    /// Retries title synchronization that was held back by a user-controlled
    /// filesystem rename. Keeping this at the orchestration boundary means an
    /// untitled draft is re-evaluated against its rebased directory rather
    /// than reusing a stale path captured before the folder move.
    pub(super) fn retry_deferred_title_sync(&mut self, cx: &mut Context<Self>) {
        if self.inline_rename.is_some() {
            return;
        }
        let deferred = std::mem::take(&mut self.title_sync_deferred);
        for id in deferred {
            self.retry_title_sync(id, cx);
        }
    }

    pub(crate) fn save_current(&mut self, cx: &mut Context<Self>) {
        self.save_active(SaveIntent::Current, cx);
    }

    pub(crate) fn save_or_prompt(&mut self, cx: &mut Context<Self>) {
        if self.sessions.active().path().is_some() {
            self.save_current(cx);
        } else {
            self.prompt_save_as(cx);
        }
    }

    fn save_active(&mut self, intent: SaveIntent, cx: &mut Context<Self>) {
        self.save_session(self.sessions.active_id(), intent, cx);
    }

    /// Hands one accepted write to the I/O boundary. The session decides whether
    /// there is a write to do at all; the view only reports what happened.
    pub(super) fn save_session(
        &mut self,
        id: SessionId,
        intent: SaveIntent,
        cx: &mut Context<Self>,
    ) {
        let Some(decision) = self
            .sessions
            .get_mut(id)
            .map(|session| session.request_save(intent))
        else {
            return;
        };
        match decision {
            SaveDecision::NeedsPath => {
                self.status = Some("Use Save As for an untitled document".to_owned());
            }
            SaveDecision::Queued => {
                self.status = Some("Save queued…".to_owned());
            }
            SaveDecision::Write(job) => {
                self.status = Some("Saving…".to_owned());
                let files = self.files.clone();
                let path = job.path.clone();
                let ticket = job.ticket;
                // Captured now, not re-read once the write lands: a
                // work-folder switch installs a fresh `SessionSet` whose ids
                // (and each fresh session's own generation) restart at the
                // same values the old one used, so an old-workspace write's
                // `SaveTicket` can coincidentally equal a new document's own
                // first ticket. `DocumentSession::adopt` bumps the
                // generation without touching the workspace at all, so an
                // in-place reopen within the same workspace is the same kind
                // of document exchange too. Binding the full instance here,
                // before the completion ever reaches `finish_save`, is what
                // tells the two apart instead of letting the ticket alone
                // decide.
                let instance = self.document_instance(id);
                cx.spawn(async move |view, cx| {
                    let result = cx
                        .background_executor()
                        .spawn(async move {
                            run_save_job(files.as_ref(), &job.path, &job.document, job.guard)
                        })
                        .await;
                    let _ = view.update(cx, |view, cx| {
                        view.finish_save(id, ticket, instance, &path, result, cx);
                    });
                })
                .detach();
            }
        }
        cx.notify();
    }

    fn finish_save(
        &mut self,
        id: SessionId,
        ticket: SaveTicket,
        instance: Option<DocumentInstance>,
        path: &Path,
        result: Result<SavedFile, SaveFailure>,
        cx: &mut Context<Self>,
    ) {
        if self.document_instance(id) != instance {
            // The document this write was for is gone, or this id now
            // names a different instance of it — a work-folder switch that
            // reused the same slot in a fresh `SessionSet`, or an in-place
            // `adopt` that replaced the document without touching the
            // workspace. Either way this completion must not touch any
            // state at all — not the file identity or saved revision a
            // `finish_save` call below would apply, and not the title-sync
            // or pending-close bookkeeping the outcome match and the tail
            // of this function would otherwise clear for it. Dropped
            // silently, the same as any other result that no longer
            // belongs to anything open.
            return;
        }
        let Some(outcome) = self
            .sessions
            .get_mut(id)
            .map(|session| session.finish_save(ticket, result))
        else {
            return;
        };
        // Issue #411: a tab's "save and close" request must only ever close
        // on this specific write landing (`SaveOutcome::Saved`). Any other
        // outcome drops the request below rather than leaving it armed,
        // because the document can already read as clean (and so pass
        // `resolve_pending_tab_close`'s dirty check) when an unrelated
        // earlier save landed first — this write's own failure, conflict, or
        // staleness must not be papered over by that coincidence, and a
        // later, unrelated save must not inherit and act on this request.
        let saved = matches!(outcome, SaveOutcome::Saved);
        match outcome {
            SaveOutcome::Saved => {
                self.status = Some("Saved".to_owned());
                self.remember_recent(path);
                cx.add_recent_document(path);
                self.retire_work_folder_draft(id, cx);
                self.apply_pending_title_sync(id, path);
                self.retry_title_sync(id, cx);
            }
            SaveOutcome::SavedStale => {
                self.status = Some("Saved snapshot; newer edits pending".to_owned());
                self.remember_recent(path);
                cx.add_recent_document(path);
                self.schedule_autosave(cx);
                self.retire_work_folder_draft(id, cx);
                self.apply_pending_title_sync(id, path);
                self.retry_title_sync(id, cx);
            }
            SaveOutcome::Conflict => {
                self.status = Some(
                    "Save refused: the file changed on disk. Save As, or save again to overwrite"
                        .to_owned(),
                );
                // The candidate name this was for was raced away; drop it
                // rather than let a later, unrelated write consume it.
                self.title_sync_pending.remove(&id);
            }
            SaveOutcome::Failed(error) => {
                self.status = Some(format!("Save failed: {error}"));
                self.title_sync_pending.remove(&id);
            }
            // The document this write belonged to is gone; nothing to report.
            SaveOutcome::Superseded => {
                self.title_sync_pending.remove(&id);
            }
        }
        if let Some(pending) = self
            .sessions
            .get_mut(id)
            .and_then(DocumentSession::take_pending_save)
        {
            self.save_session(id, pending, cx);
        }
        if saved {
            self.resolve_pending_tab_close(id, cx);
        } else {
            self.tab_close_after_save.remove(&id);
        }
        cx.notify();
    }

    pub(crate) fn prompt_save_as(&mut self, cx: &mut Context<Self>) {
        let directory = self
            .sessions
            .active()
            .file()
            .directory()
            .map(Path::to_path_buf)
            .or_else(|| {
                self.work_folder
                    .as_ref()
                    .map(|folder| folder.root().to_path_buf())
            })
            .unwrap_or_else(|| PathBuf::from("."));
        let receiver = cx.prompt_for_new_path(&directory, Some("Untitled.md"));
        cx.spawn(async move |view, cx| match receiver.await {
            Ok(Ok(Some(path))) => {
                let _ = view.update(cx, |view, cx| view.save_active(SaveIntent::To(path), cx));
            }
            Ok(Err(error)) => {
                let _ = view.update(cx, |view, cx| {
                    view.status = Some(format!("Save As failed: {error}"));
                    cx.notify();
                });
            }
            _ => {}
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hane_session::FileIdentity;

    fn rename_test_root(label: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQUENCE: AtomicU64 = AtomicU64::new(0);
        std::env::temp_dir().join(format!(
            "hane-rename-adopt-{label}-{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ))
    }

    // PR #416 review follow-up: `DocumentSession::adopt` bumps the
    // generation without touching the workspace at all — an in-place
    // reopen into the same tab, not a work-folder switch.
    // `begin_title_rename`'s completion used to compare only the
    // workspace, so a rename started before an in-place `adopt` could
    // still reach `finish_title_rename` and release the save slot, drain
    // a save queued behind it, and resolve the close request of the
    // document the `adopt` replaced it with.
    #[gpui::test]
    fn a_stale_rename_completion_from_before_an_in_place_adopt_does_not_touch_the_replacement_documents_bookkeeping(
        cx: &mut gpui::TestAppContext,
    ) {
        let root = rename_test_root("stale-rename-adopt");
        std::fs::create_dir_all(&root).unwrap();
        let old_path = root.join("Old.md");
        std::fs::write(&old_path, "body\n").unwrap();
        let loaded = OsFileService.load(&old_path).unwrap();

        let (view, cx) = cx.add_window_view(|_, cx| {
            EditorView::from_sessions(
                SessionSet::with_loaded(loaded),
                Arc::new(OsFileService),
                StateStores::memory(),
                cx,
            )
        });
        cx.run_until_parked();

        // Reserves the save slot and spawns the rename's background
        // probe/IO synchronously, with no debounce timer involved — the
        // title passed directly, bypassing `decide_title_sync`, since
        // only `begin_title_rename`'s own race protection is under test
        // here. Leaves the result unpolled: nothing here calls
        // `cx.run_until_parked()` before the `adopt` below replaces the
        // document in place.
        let id = view.update(cx, |view, cx| {
            let id = view.sessions.active_id();
            view.begin_title_rename(id, "New".to_owned(), cx);
            assert!(view.sessions.active().save_in_flight());
            id
        });

        // Replaces the document in place — same `SessionId`, same
        // workspace, generation bumped — while the rename above is still
        // unpolled. The replacement document then reserves its own save
        // slot and queues a save behind it, entirely independent of the
        // stale rename above.
        let new_instance = view.update(cx, |view, _cx| {
            let session = view.sessions.active_mut();
            session.adopt(LoadedFile {
                document: RopeBuffer::from_text("replaced\n"),
                identity: FileIdentity::lexical(PathBuf::from("replaced.md")),
                stamp: None,
            });
            assert!(session.begin_rename().is_some());
            assert!(matches!(
                session.request_save(SaveIntent::Current),
                SaveDecision::Queued
            ));
            DocumentInstance {
                generation: session.generation(),
                workspace: view.work_folder_generation,
            }
        });
        view.update(cx, |view, _cx| {
            view.title_sync_in_flight.insert(id);
            view.tab_close_after_save.insert(id, new_instance);
        });

        cx.run_until_parked();

        assert!(
            !old_path.exists(),
            "the stale rename itself must still have landed on disk"
        );

        view.update(cx, |view, _cx| {
            let session = view
                .sessions
                .get_mut(id)
                .expect("the replacement document must still be open");
            assert_eq!(
                session.path(),
                Some(PathBuf::from("replaced.md")).as_deref(),
                "the stale rename completion must not touch the replacement document's path"
            );
            assert!(
                session.save_in_flight(),
                "the stale rename completion must not release the replacement document's own \
                 save slot"
            );
            assert!(
                matches!(session.take_pending_save(), Some(SaveIntent::Current)),
                "the stale rename completion must not drain the replacement document's own \
                 queued save"
            );
            assert!(
                view.title_sync_in_flight.contains(&id),
                "the stale rename completion must not clear a title-sync in-flight marker \
                 armed for the replacement document"
            );
            assert_eq!(
                view.tab_close_after_save.get(&id),
                Some(&new_instance),
                "the stale rename completion must not clear a close-and-save request armed \
                 for the replacement document"
            );
        });

        std::fs::remove_dir_all(&root).unwrap();
    }
}
