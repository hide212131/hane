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
        // Captured now, not re-read once the timer fires: a work-folder
        // switch installs a fresh `SessionSet` whose ids (and each fresh
        // session's own generation) restart at the same values the old one
        // used, so `AutosaveTicket`'s own generation field — scoped to a
        // single document, not a workspace — can coincidentally still read
        // as current for an unrelated document that now holds `id`. Only
        // the full instance, including the workspace epoch, tells them
        // apart.
        let instance = self.document_instance(id);
        cx.spawn(async move |view, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(750))
                .await;
            let (should_save, composing) = view
                .read_with(cx, |view, _| {
                    let current = view.document_instance(id) == instance
                        && view.sessions.active_id() == id
                        && view
                            .sessions
                            .active()
                            .autosave_is_current(ticket, view.settings.autosave);
                    // The marked text is not committed yet, so writing it
                    // would put an unconfirmed composition on disk.
                    let composing = view.sessions.active().editor().ime().is_some();
                    (current && !composing, current && composing)
                })
                .unwrap_or((false, false));
            if should_save {
                let _ = view.update(cx, |view, cx| view.save_current(cx));
            } else if composing {
                // Try again once the composition may have ended; the edit
                // that armed this timer is still unsaved.
                let _ = view.update(cx, |view, cx| view.schedule_autosave(cx));
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
        // Captured now, not re-read once the timer fires: a work-folder
        // switch or an in-place `adopt` can leave `id` naming an unrelated
        // document whose own revision coincidentally matches this one (both
        // freshly created documents restart their counters at the same
        // values). `work_folder_drafts.contains_key` alone cannot tell that
        // unrelated document's own draft apart from the one this timer was
        // armed for, so the completion below also requires the draft id
        // itself, not merely some draft, to still be the one captured here.
        let instance = self.document_instance(id);
        let draft_store = self.draft_store.clone();
        cx.spawn(async move |view, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(750))
                .await;
            let text = view
                .read_with(cx, |view, _| {
                    if view.document_instance(id) != instance {
                        return None;
                    }
                    let session = view.sessions.get(id)?;
                    let draft = view.work_folder_drafts.get(&id)?;
                    let current = session.revision() == revision && draft.draft_id == draft_id;
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

    /// The update-restart variant of `flush_pending_drafts`: every pending
    /// draft is written and every failure is reported, instead of being
    /// ignored. `Ok` carries the number of drafts written. The ordinary quit
    /// path keeps using the best-effort method above.
    pub(super) fn flush_pending_drafts_checked(&self) -> Result<usize, Vec<String>> {
        if self.work_folder_drafts.is_empty() {
            return Ok(0);
        }
        let Some(root) = self.work_folder.as_ref().map(WorkFolder::root) else {
            return Err(vec!["作業フォルダーを特定できません".to_owned()]);
        };
        let mut written = 0;
        let mut failures = Vec::new();
        for (&id, draft) in &self.work_folder_drafts {
            let Some(session) = self.sessions.get(id) else {
                continue;
            };
            let text = session.editor().document().full_text();
            match self.draft_store.write(root, draft.draft_id, &text) {
                Ok(()) => written += 1,
                Err(error) => failures.push(format!("{}: {error}", session.label())),
            }
        }
        if failures.is_empty() {
            Ok(written)
        } else {
            Err(failures)
        }
    }

    /// Issue #6: arms the debounce timer that keeps a work-folder note's
    /// filename following its first H1. A no-op outside a work folder.
    pub(super) fn schedule_title_sync(&mut self, cx: &mut Context<Self>) {
        if self.work_folder.is_none() {
            return;
        }
        let id = self.sessions.active_id();
        let revision = self.sessions.active().revision();
        // Captured now, not re-read once the timer fires: a work-folder
        // switch or an in-place `adopt` can leave `id` naming an unrelated
        // document whose own revision coincidentally matches this one (both
        // freshly created documents restart their counters at the same
        // values), which `title_sync_scheduled`'s revision-only check
        // cannot tell apart on its own. Only the full instance, including
        // the workspace epoch, tells them apart; it must be checked before
        // this completion touches the scheduled-map entry or decides
        // anything for `id`, not only inside `run_title_sync` afterwards.
        let Some(instance) = self.document_instance(id) else {
            return;
        };
        self.title_sync_scheduled.insert(id, revision);
        cx.spawn(async move |view, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(750))
                .await;
            let _ = view.update(cx, |view, cx| {
                if view.document_instance(id) != Some(instance) {
                    return;
                }
                if view.title_sync_scheduled.get(&id).copied() == Some(revision) {
                    view.title_sync_scheduled.remove(&id);
                    view.run_title_sync(id, instance.generation, revision, cx);
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
        // Captured now, before any `await`: a work-folder switch installs a
        // fresh `SessionSet` whose ids restart at the same values the old
        // one used, and `DocumentSession::adopt` bumps the generation
        // without touching the workspace at all, so either kind of document
        // exchange can leave this probe's completion naming an id that now
        // belongs to an unrelated document — one with no path and no save
        // in flight of its own yet, which `should_defer_h1_create` alone
        // cannot tell apart from the untitled document this probe actually
        // started for. Binding the full instance here, before the probe
        // ever reaches its completion below, is what tells them apart,
        // the same as `save_session` and `begin_title_rename` already do
        // for their own completions.
        let instance = self.document_instance(id);
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
                if view.document_instance(id) != instance {
                    // The document this probe was started for is gone, or
                    // this id now names a different instance of it. Nothing
                    // may be touched on behalf of whatever unrelated
                    // document now holds `id`: not its in-flight marker,
                    // not a title-sync pending entry, and no write is
                    // started against it — a fresh `run_title_sync` for
                    // that document decides its own title-sync from
                    // scratch.
                    return;
                }
                view.title_sync_in_flight.remove(&id);
                // A session that no longer exists (closed in the meantime)
                // defers the same as one that reports
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
        let Some(from) = self
            .sessions
            .get(id)
            .and_then(|session| session.path().map(Path::to_path_buf))
        else {
            return;
        };
        let Some(root) = from.parent().map(Path::to_path_buf) else {
            return;
        };
        if self.folder_trash_blocks(&from, false) {
            return;
        }
        let Some(session) = self.sessions.get_mut(id) else {
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
        // The rename never moves the file to a different directory (`target`
        // below is always `root.join(candidate)`), so the containing folder
        // is the same before and after it lands; computed now, off the tree
        // the rename was requested against.
        let ancestor_folders = self
            .work_folder
            .as_ref()
            .map(|folder| folder.ancestor_folder_paths(&from))
            .unwrap_or_default();
        cx.spawn(async move |view, cx| {
            let probe_files = files.clone();
            let probe_root = root.clone();
            let rename_from = from.clone();
            let (target, rename_result, ancestors) = cx
                .background_executor()
                .spawn(async move {
                    let candidate = unique_markdown_filename(&probe_title, |name| {
                        let candidate = probe_root.join(name);
                        candidate != probe_from && probe_files.stamp(&candidate).is_some()
                    });
                    let target = probe_root.join(candidate);
                    let result = probe_files.rename(&rename_from, &target);
                    // The containing directory's own entry changed with the
                    // rename (an entry removed under the old name, added
                    // under the new one), so its real modified time is
                    // acquired here too. A failed rename never touched the
                    // filesystem, so nothing here changed either.
                    let ancestors = if result.is_ok() {
                        stamp_ancestor_folders(probe_files.as_ref(), &ancestor_folders)
                    } else {
                        Vec::new()
                    };
                    (target, result, ancestors)
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
                    ancestors,
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
            ancestors,
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
                self.touch_work_folder_ancestors(&ancestors);
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
        self.content_search_workspace_changed(cx);
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
        if let Some(title) = self.title_sync_pending.remove(&id)
            && let Some(session) = self.sessions.get_mut(id)
        {
            session.note_auto_named(title);
        }
        // Every landed write is indexed, not only a title-sync first write:
        // a Save As into the work folder lands a path the tree has never
        // seen. `insert` ignores already-indexed and out-of-folder paths.
        if let Some(folder) = self.work_folder.as_mut() {
            folder.insert(path.to_path_buf());
        }
    }

    /// Carries the modified time a just-landed save or autosave produced
    /// into the retained work-folder tree — the same no-rescan update
    /// `rename` and `insert`/`insert_folder` already give a move or
    /// creation — so an `Updated` sidebar sort reflects this write
    /// immediately rather than only after the next full rescan. Runs after
    /// `apply_pending_title_sync` so a note's first write (which both
    /// indexes it and produces its first stamp) picks up the time too, not
    /// just an ordinary save of an already-indexed note. A session with no
    /// on-disk stamp, or a `path` the tree was never indexed with, is a
    /// no-op. `ancestors` (acquired off the UI thread alongside the save
    /// itself) carries the same update to the containing folder whose own
    /// directory entry changed too — see `WorkFolder::ancestor_folder_paths`.
    fn touch_saved_work_folder_entry(
        &mut self,
        path: &Path,
        leaf: Option<(Option<SystemTime>, u64)>,
        ancestors: &[(PathBuf, Option<SystemTime>, u64)],
    ) {
        if let (Some(folder), Some((modified, sequence))) = (self.work_folder.as_mut(), leaf) {
            folder.touch(path, modified, sequence);
        }
        self.touch_work_folder_ancestors(ancestors);
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
        // A folder is being moved to the OS Trash: a write under it would
        // recreate the original path or mark the session Present again once
        // the move lands. The content stays in the session, unsaved.
        if self.folder_trash_in_flight.is_some() {
            let target_blocked = match &intent {
                SaveIntent::To(path) | SaveIntent::CreateNew(path) => {
                    self.folder_trash_blocks(path, false)
                }
                _ => false,
            };
            let session_blocked = self
                .sessions
                .get(id)
                .and_then(DocumentSession::path)
                .is_some_and(|path| self.folder_trash_blocks(path, false));
            if target_blocked || session_blocked {
                self.title_sync_pending.remove(&id);
                self.tab_close_after_save.remove(&id);
                self.status = Some(
                    "フォルダをゴミ箱へ移動中のため保存できません。完了後にやり直してください"
                        .to_owned(),
                );
                cx.notify();
                return;
            }
        }
        let Some(decision) = self
            .sessions
            .get_mut(id)
            .map(|session| session.request_save(intent))
        else {
            return;
        };
        match decision {
            SaveDecision::NeedsPath => {
                // Guidance, not a save state: the footer's save display is
                // derived from the document itself. Another document's
                // request names its target.
                let target = (id != self.sessions.active_id())
                    .then(|| self.sessions.get(id).map(DocumentSession::label))
                    .flatten();
                self.status = Some(match target {
                    Some(label) => {
                        format!("{label}: 保存先が決まっていません。名前を付けて保存してください")
                    }
                    None => "保存先が決まっていません。名前を付けて保存してください".to_owned(),
                });
            }
            // The footer derives "保存処理中…" from the document's own
            // `save_in_flight()`, so queued and started writes need no
            // message of their own.
            SaveDecision::Queued => {}
            SaveDecision::Write(job) => {
                // A started write replaces any older transient message, the
                // way the former "Saving…" text did; the save problem of the
                // document is deliberately left alone until a save succeeds.
                self.status = None;
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
                // Computed now, off the tree the save was requested against,
                // rather than re-borrowing `self.work_folder` once the write
                // lands: a workspace switch in the meantime simply leaves
                // these paths absent from whatever tree `finish_save` then
                // finds, a no-op for `touch_work_folder_ancestors`.
                let ancestor_folders = self
                    .work_folder
                    .as_ref()
                    .map(|folder| folder.ancestor_folder_paths(&path))
                    .unwrap_or_default();
                cx.spawn(async move |view, cx| {
                    let (result, leaf, ancestors) = cx
                        .background_executor()
                        .spawn(async move {
                            let result =
                                run_save_job(files.as_ref(), &job.path, &job.document, job.guard);
                            // The saved file's own modified time is read and
                            // sequenced here, at the point of the write,
                            // rather than when this completion reaches the
                            // UI thread: a rescan that observed a later
                            // external change and was applied first holds a
                            // newer sequence, so this older read is dropped.
                            let leaf = if result.is_ok() {
                                Some(stamp_with_sequence(files.as_ref(), &job.path))
                            } else {
                                None
                            };
                            // The containing directory's own entry changed
                            // alongside the leaf's write (an atomic save
                            // replaces the target via a temp-file rename),
                            // so its real modified time is acquired here too
                            // rather than left to the next full rescan. A
                            // failed write never touched the filesystem, so
                            // nothing here changed either.
                            let ancestors = if result.is_ok() {
                                stamp_ancestor_folders(files.as_ref(), &ancestor_folders)
                            } else {
                                Vec::new()
                            };
                            (result, leaf, ancestors)
                        })
                        .await;
                    let _ = view.update(cx, |view, cx| {
                        let attempt = SaveAttempt {
                            id,
                            ticket,
                            instance,
                            path,
                            leaf,
                            ancestors,
                        };
                        view.finish_save(attempt, result, cx);
                    });
                })
                .detach();
            }
        }
        cx.notify();
    }

    fn finish_save(
        &mut self,
        attempt: SaveAttempt,
        result: Result<SavedFile, SaveFailure>,
        cx: &mut Context<Self>,
    ) {
        let SaveAttempt {
            id,
            ticket,
            instance,
            path,
            leaf,
            ancestors,
        } = attempt;
        let path = path.as_path();
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
                self.status = None;
                self.save_problems.remove(&id);
                self.remember_recent(path);
                cx.add_recent_document(path);
                self.retire_work_folder_draft(id, cx);
                self.apply_pending_title_sync(id, path);
                self.touch_saved_work_folder_entry(path, leaf, &ancestors);
                self.retry_title_sync(id, cx);
            }
            SaveOutcome::SavedStale => {
                // The snapshot landed, so the earlier problem is resolved;
                // the edits that arrived meanwhile show as "未保存".
                self.status = None;
                self.save_problems.remove(&id);
                self.remember_recent(path);
                cx.add_recent_document(path);
                self.schedule_autosave(cx);
                self.retire_work_folder_draft(id, cx);
                self.apply_pending_title_sync(id, path);
                self.touch_saved_work_folder_entry(path, leaf, &ancestors);
                self.retry_title_sync(id, cx);
            }
            SaveOutcome::Conflict => {
                self.status = None;
                if let Some(instance) = instance {
                    self.record_save_problem(
                        id,
                        instance,
                        Some(path.to_path_buf()),
                        DocumentSaveProblem::Conflict,
                    );
                }
                // The candidate name this was for was raced away; drop it
                // rather than let a later, unrelated write consume it.
                self.title_sync_pending.remove(&id);
            }
            SaveOutcome::Failed(error) => {
                self.status = None;
                if let Some(instance) = instance {
                    self.record_save_problem(
                        id,
                        instance,
                        Some(path.to_path_buf()),
                        DocumentSaveProblem::Failed(error.to_string()),
                    );
                }
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
        self.prompt_save_as_for(self.sessions.active_id(), cx);
    }

    /// Opens the native Save As dialog for the session `id`, which need not be
    /// the active one (a sidebar row's menu targets the row, not the tab). The
    /// answer is applied through `handle_save_as_response`, bound to this
    /// session's instance.
    pub(super) fn prompt_save_as_for(&mut self, id: SessionId, cx: &mut Context<Self>) {
        let Some(session) = self.sessions.get(id) else {
            return;
        };
        let directory = session
            .file()
            .directory()
            .map(Path::to_path_buf)
            .or_else(|| {
                self.work_folder
                    .as_ref()
                    .map(|folder| folder.root().to_path_buf())
            })
            .unwrap_or_else(|| PathBuf::from("."));
        // Captured before the native dialog is even shown — not re-read once
        // it resolves — the same reason `prompt_save_as_for_close` captures
        // its own instance: this (native, unboundedly long-lived) dialog's
        // eventual answer must not be mistaken for an answer about whatever
        // document happens to be active by the time it resolves, whether
        // because a work-folder switch or an in-place reopen replaced this
        // one, or simply because the user switched to another tab while the
        // dialog was still open.
        let instance = self.document_instance(id);
        let receiver = cx.prompt_for_new_path(&directory, Some("Untitled.md"));
        self.save_as_dialogs_open += 1;
        cx.spawn(async move |view, cx| {
            let response = match receiver.await {
                Ok(Ok(Some(path))) => SaveAsResponse::Chosen(path),
                Ok(Ok(None)) => SaveAsResponse::Canceled,
                Ok(Err(error)) => SaveAsResponse::Failed(error.to_string()),
                Err(_) => SaveAsResponse::Canceled,
            };
            let _ = view.update(cx, |view, cx| {
                // Released before the instance gate: the dialog is over even
                // when its answer no longer belongs to any open document.
                view.save_as_dialogs_open = view.save_as_dialogs_open.saturating_sub(1);
                view.handle_save_as_response(id, instance, response, cx);
            });
        })
        .detach();
    }

    /// The single entry point every resolution of the Save As dialog
    /// `prompt_save_as` opens must pass through, mirroring
    /// `handle_save_as_for_close_response`'s contract: only the document
    /// instance the dialog was actually opened for may act on its answer.
    /// By the time this runs the id may already belong to an unrelated,
    /// newer document — a work-folder switch, an in-place `adopt`, or
    /// simply the user having switched to another, unrelated tab while the
    /// dialog was open — and none of them, not a path landing on disk and
    /// not even the status message, may be touched on behalf of a dialog
    /// that document never opened. In particular, a chosen path is saved to
    /// the original session by id, never to whatever session happens to be
    /// active when the user finally answers.
    pub(super) fn handle_save_as_response(
        &mut self,
        id: SessionId,
        instance: Option<DocumentInstance>,
        response: SaveAsResponse,
        cx: &mut Context<Self>,
    ) {
        if self.document_instance(id) != instance {
            return;
        }
        match response {
            SaveAsResponse::Chosen(path) => {
                self.save_session(id, SaveIntent::To(path), cx);
            }
            SaveAsResponse::Failed(error) => {
                // Only recorded for the document the dialog was opened for
                // (the instance gate above); no problem is raised by a
                // cancel, and an earlier unresolved one is not cleared.
                if let Some(instance) = instance {
                    self.record_save_problem(
                        id,
                        instance,
                        None,
                        DocumentSaveProblem::SaveAsDialogFailed(error),
                    );
                }
                cx.notify();
            }
            SaveAsResponse::Canceled => {}
        }
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

    // Issue #585: while a folder is moving to the OS Trash, a Save (or Save
    // As into that subtree) must not start: a late write would recreate the
    // original path. The edit stays in the session, unsaved, and a rename of
    // the folder is refused too.
    #[gpui::test]
    fn saving_and_renaming_under_a_folder_being_trashed_are_refused(
        cx: &mut gpui::TestAppContext,
    ) {
        use hane_session::WorkFolderScanner as _;

        let root = rename_test_root("folder-trash-race");
        let folder_path = root.join("dev");
        std::fs::create_dir_all(&folder_path).unwrap();
        let note = folder_path.join("Note.md");
        std::fs::write(&note, "body\n").unwrap();
        let loaded = OsFileService.load(&note).unwrap();
        let work_folder = hane_session::OsWorkFolderScanner.scan(&root).unwrap();

        let (view, cx) = cx.add_window_view(|_, cx| {
            EditorView::from_sessions(
                SessionSet::with_loaded(loaded),
                Arc::new(OsFileService),
                StateStores::memory(),
                cx,
            )
        });
        view.update(cx, |view, _cx| {
            view.work_folder = Some(work_folder);
            let _ = view.sessions.active_mut().editor_mut().insert_text("more\n");
            view.folder_trash_in_flight = Some(folder_path.clone());
        });

        // The trash move removes the folder while the save is attempted.
        std::fs::remove_dir_all(&folder_path).unwrap();
        view.update(cx, |view, cx| {
            let id = view.sessions.active_id();
            view.save_session(id, SaveIntent::Current, cx);
            view.save_session(id, SaveIntent::To(folder_path.join("Other.md")), cx);
            assert!(!view.sessions.active().save_in_flight());
            assert!(view.sessions.active().is_dirty());
            assert!(view.inline_rename_has_background_conflict(
                &folder_path,
                InlineRenameKind::Folder
            ));
        });
        cx.run_until_parked();

        assert!(
            !folder_path.exists(),
            "a refused save must not recreate the trashed folder"
        );
        view.update(cx, |view, _cx| {
            assert!(view.sessions.active().is_dirty());
        });

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[gpui::test]
    fn new_entries_under_a_folder_being_trashed_are_refused(cx: &mut gpui::TestAppContext) {
        use hane_session::WorkFolderScanner as _;

        let root = rename_test_root("folder-trash-new-entry");
        let folder_path = root.join("dev");
        std::fs::create_dir_all(&folder_path).unwrap();
        let work_folder = hane_session::OsWorkFolderScanner.scan(&root).unwrap();

        let (view, cx) = cx.add_window_view(|_, cx| {
            EditorView::from_sessions(
                SessionSet::with_untitled("", "Untitled"),
                Arc::new(OsFileService),
                StateStores::memory(),
                cx,
            )
        });
        view.update(cx, |view, cx| {
            view.work_folder = Some(work_folder);
            view.selected_folder = Some(folder_path.clone());
            view.folder_trash_in_flight = Some(folder_path.clone());
            let sessions_before = view.sessions.len();

            view.new_work_folder_note(cx);
            view.new_work_folder_folder(cx);

            assert_eq!(view.sessions.len(), sessions_before);
            assert!(view.work_folder_drafts.is_empty());
            assert!(view.pending_new_folders.is_empty());
            assert!(view.status.is_some());
        });
        cx.run_until_parked();
        assert!(!folder_path.join("New Folder").exists());

        std::fs::remove_dir_all(&root).unwrap();
    }

    // Issue #585: a folder create still pending under a folder must stop the
    // folder's Trash move, otherwise the late create would recreate the
    // moved path and the tree entry.
    #[gpui::test]
    fn trashing_a_folder_with_a_pending_folder_create_is_refused(cx: &mut gpui::TestAppContext) {
        use hane_session::WorkFolderScanner as _;

        let root = rename_test_root("folder-trash-pending-create");
        let folder_path = root.join("dev");
        std::fs::create_dir_all(&folder_path).unwrap();
        let work_folder = hane_session::OsWorkFolderScanner.scan(&root).unwrap();

        let (view, cx) = cx.add_window_view(|_, cx| {
            EditorView::from_sessions(
                SessionSet::with_untitled("", "Untitled"),
                Arc::new(OsFileService),
                StateStores::memory(),
                cx,
            )
        });
        view.update(cx, |view, cx| {
            view.work_folder = Some(work_folder);
            view.pending_new_folders
                .insert(folder_path.join("New Folder"));

            view.sidebar_context_trash_folder(folder_path.clone(), cx);

            assert!(view.folder_trash_in_flight.is_none());
            assert!(view.pending_new_folders.contains(&folder_path.join("New Folder")));
            assert!(view
                .status
                .as_deref()
                .is_some_and(|status| status.contains("作成中")));
        });
        cx.run_until_parked();
        assert!(folder_path.exists(), "a refused trash must not move the folder");

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[gpui::test]
    fn drafts_in_a_trashed_folder_are_retargeted_to_the_nearest_surviving_ancestor(
        cx: &mut gpui::TestAppContext,
    ) {
        use hane_session::WorkFolderScanner as _;

        let root = rename_test_root("folder-trash-drafts");
        let parent = root.join("a");
        let trashed = parent.join("b");
        let other = root.join("c");
        std::fs::create_dir_all(trashed.join("deep")).unwrap();
        std::fs::create_dir_all(&other).unwrap();
        let work_folder = hane_session::OsWorkFolderScanner.scan(&root).unwrap();

        let (view, cx) = cx.add_window_view(|_, cx| {
            EditorView::from_sessions(
                SessionSet::with_untitled("", "Untitled"),
                Arc::new(OsFileService),
                StateStores::memory(),
                cx,
            )
        });
        view.update(cx, |view, _cx| {
            view.work_folder = Some(work_folder);
            let inside = view.sessions.active_id();
            view.work_folder_drafts.insert(
                inside,
                WorkFolderDraft {
                    draft_id: DraftId::generate(),
                    target_directory: trashed.join("deep"),
                },
            );
            let outside = view.sessions.open_untitled("# keep", "Untitled");
            view.work_folder_drafts.insert(
                outside,
                WorkFolderDraft {
                    draft_id: DraftId::generate(),
                    target_directory: other.clone(),
                },
            );

            view.work_folder.as_mut().unwrap().remove_folder(&trashed);
            view.retarget_drafts_after_folder_trash(&trashed);

            assert_eq!(view.work_folder_drafts[&inside].target_directory, parent);
            assert_eq!(view.work_folder_drafts[&outside].target_directory, other);
            assert!(view.sessions.get(outside).is_some());
        });

        std::fs::remove_dir_all(&root).unwrap();
    }

    // Issue #236: a Save As into the work folder must reach the sidebar index
    // without waiting for a full rescan, for an already-saved document
    // (`Saved`) and for one with newer edits pending (`SavedStale`) alike.
    #[gpui::test]
    fn save_as_into_the_work_folder_indexes_the_destination_without_a_rescan(
        cx: &mut gpui::TestAppContext,
    ) {
        use hane_session::WorkFolderScanner as _;

        let root = rename_test_root("save-as-index");
        std::fs::create_dir_all(&root).unwrap();
        let old_path = root.join("Old.md");
        std::fs::write(&old_path, "body\n").unwrap();
        let loaded = OsFileService.load(&old_path).unwrap();
        let work_folder = hane_session::OsWorkFolderScanner.scan(&root).unwrap();

        let (view, cx) = cx.add_window_view(|_, cx| {
            EditorView::from_sessions(
                SessionSet::with_loaded(loaded),
                Arc::new(OsFileService),
                StateStores::memory(),
                cx,
            )
        });
        view.update(cx, |view, _cx| {
            view.work_folder = Some(work_folder);
        });

        let saved_path = root.join("Saved.md");
        view.update(cx, |view, cx| {
            let id = view.sessions.active_id();
            view.save_session(id, SaveIntent::To(saved_path.clone()), cx);
        });
        cx.run_until_parked();
        view.update(cx, |view, _cx| {
            let folder = view.work_folder.as_ref().unwrap();
            assert!(folder.entry_for_path(&saved_path).is_some());
            assert!(folder.entry_for_path(&old_path).is_some());
        });

        // Edit while a Save As is in flight so the write lands as `SavedStale`.
        let stale_path = root.join("Stale.md");
        view.update(cx, |view, cx| {
            let id = view.sessions.active_id();
            view.save_session(id, SaveIntent::To(stale_path.clone()), cx);
            let _ = view.sessions.active_mut().editor_mut().insert_text("more\n");
        });
        cx.run_until_parked();
        view.update(cx, |view, _cx| {
            let folder = view.work_folder.as_ref().unwrap();
            assert!(folder.entry_for_path(&stale_path).is_some());
        });

        std::fs::remove_dir_all(&root).unwrap();
    }
}
