use super::*;

impl EditorView {
    pub(crate) fn inline_rename_active(&self) -> bool {
        self.inline_rename.is_some()
    }

    pub(crate) fn inline_rename_render_state(&self) -> Option<InlineRenameRenderState> {
        self.inline_rename
            .as_ref()
            .map(|rename| InlineRenameRenderState {
                text: rename.input.text().to_owned(),
                selected_range: rename.input.selected_range(),
                selection_reversed: rename.input.selection_reversed(),
                marked_range: rename.input.marked_range(),
            })
    }

    pub(crate) fn set_inline_rename_input_bounds(&mut self, bounds: Bounds<Pixels>) {
        self.inline_rename_input_bounds = Some(bounds);
    }

    /// Converts a native mouse position into the UTF-16 offset expected by the
    /// platform input handler, using the same shaped line that is painted for
    /// the inline rename field.
    pub(crate) fn inline_rename_character_index_for_point(
        &self,
        position: gpui::Point<Pixels>,
        window: &mut Window,
    ) -> Option<usize> {
        let bounds = self.inline_rename_input_bounds?;
        let state = self.inline_rename_render_state()?;
        let line = shape_inline_rename_line(&state, window);
        let x = if position.x <= bounds.left() {
            px(0.0)
        } else {
            position.x - bounds.left()
        };
        let byte = line.closest_index_for_x(x).min(state.text.len());
        Some(utf16_offset_from_byte(&state.text, byte))
    }

    pub(crate) fn move_inline_rename_to_point(
        &mut self,
        position: gpui::Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(index) = self.inline_rename_character_index_for_point(position, window) else {
            return;
        };
        let Some(rename) = self.inline_rename.as_mut() else {
            return;
        };
        if rename.pending || rename.input.has_composition() {
            return;
        }
        rename.input.set_caret_utf16(index);
        cx.notify();
    }

    pub(crate) fn inline_rename_has_composition(&self) -> bool {
        self.inline_rename
            .as_ref()
            .is_some_and(|rename| rename.input.has_composition())
    }

    pub(crate) fn inline_rename_text_for_range(
        &self,
        range_utf16: Range<usize>,
    ) -> Option<(String, Range<usize>)> {
        let rename = self.inline_rename.as_ref()?;
        Some(rename.input.text_for_utf16_range(range_utf16))
    }

    pub(crate) fn inline_rename_selection(&self) -> Option<(Range<usize>, bool)> {
        self.inline_rename
            .as_ref()
            .map(|rename| rename.input.selection_utf16())
    }

    pub(crate) fn inline_rename_marked_range(&self) -> Option<Range<usize>> {
        self.inline_rename.as_ref()?.input.marked_range_utf16()
    }

    /// Whether a platform text callback (`replace_text_in_range`,
    /// `replace_and_mark_text_in_range`) must be ignored. Text received while
    /// the rename is pending is never applied, and the platform may still
    /// hold it and commit it after the rename ended (to the body or back into
    /// the rename field). So the first such callback arms a guard that drops
    /// text callbacks until the next keystroke.
    pub(crate) fn drop_input_after_pending_rename(&self) -> bool {
        if self
            .inline_rename
            .as_ref()
            .is_some_and(|rename| rename.pending)
        {
            self.inline_rename_swallowed_input.set(true);
        }
        self.inline_rename_swallowed_input.get()
    }

    /// Whether the late-text guard is armed without arming it. An Enter seen
    /// while armed can only confirm a composition whose text was held back
    /// during the pending rename, so it must not edit the body.
    pub(crate) fn late_input_guard_armed(&self) -> bool {
        self.inline_rename_swallowed_input.get()
    }

    pub(crate) fn replace_inline_rename_text(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(rename) = self.inline_rename.as_mut() else {
            return false;
        };
        if rename.pending {
            return true;
        }
        rename.input.replace(range_utf16, new_text);
        cx.notify();
        true
    }

    pub(crate) fn replace_and_mark_inline_rename_text(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selected_range_utf16: Option<Range<usize>>,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(rename) = self.inline_rename.as_mut() else {
            return false;
        };
        if rename.pending {
            return true;
        }
        rename
            .input
            .replace_and_mark(range_utf16, new_text, new_selected_range_utf16);
        cx.notify();
        true
    }

    pub(crate) fn commit_inline_rename_composition(&mut self, cx: &mut Context<Self>) {
        let Some(rename) = self.inline_rename.as_mut() else {
            return;
        };
        if rename.input.commit_composition() {
            cx.notify();
        }
    }

    pub(crate) fn cancel_inline_rename_composition(&mut self, cx: &mut Context<Self>) {
        let Some(rename) = self.inline_rename.as_mut() else {
            return;
        };
        if rename.input.cancel_composition() {
            cx.notify();
        }
    }

    /// Platform cancel (an empty, rangeless replacement during composition).
    /// While a rename is pending the field is protected, so it is left as is.
    pub(crate) fn cancel_inline_rename_composition_unless_pending(
        &mut self,
        cx: &mut Context<Self>,
    ) {
        if self.inline_rename.as_ref().is_some_and(|rename| rename.pending) {
            return;
        }
        self.cancel_inline_rename_composition(cx);
    }

    pub(crate) fn selected_inline_rename_text(&self) -> Option<String> {
        let rename = self.inline_rename.as_ref()?;
        rename.input.selected_text().map(str::to_owned)
    }

    pub(crate) fn move_inline_rename_left(&mut self, extend: bool, cx: &mut Context<Self>) {
        self.move_inline_rename_horizontal(false, extend, cx);
    }

    pub(crate) fn move_inline_rename_right(&mut self, extend: bool, cx: &mut Context<Self>) {
        self.move_inline_rename_horizontal(true, extend, cx);
    }

    fn move_inline_rename_horizontal(&mut self, right: bool, extend: bool, cx: &mut Context<Self>) {
        let Some(rename) = self.inline_rename.as_mut() else {
            return;
        };
        if rename.pending {
            return;
        }
        rename.input.move_horizontal(right, extend);
        cx.notify();
    }

    /// Shift+Left / Shift+Right entry. Unlike `move_inline_rename_left` with
    /// `extend`, this entry has never checked `pending`; keep that difference.
    pub(crate) fn select_inline_rename_left(&mut self, cx: &mut Context<Self>) {
        if let Some(rename) = self.inline_rename.as_mut() {
            rename.input.move_horizontal(false, true);
            cx.notify();
        }
    }

    pub(crate) fn select_inline_rename_right(&mut self, cx: &mut Context<Self>) {
        if let Some(rename) = self.inline_rename.as_mut() {
            rename.input.move_horizontal(true, true);
            cx.notify();
        }
    }

    pub(crate) fn select_all_inline_rename(&mut self, cx: &mut Context<Self>) {
        if let Some(rename) = self.inline_rename.as_mut()
            && !rename.pending
        {
            rename.input.select_all();
            cx.notify();
        }
    }

    pub(crate) fn select_inline_rename_home(&mut self, cx: &mut Context<Self>) {
        if let Some(rename) = self.inline_rename.as_mut()
            && !rename.pending
        {
            rename.input.move_home(true);
            cx.notify();
        }
    }

    pub(crate) fn select_inline_rename_end(&mut self, cx: &mut Context<Self>) {
        if let Some(rename) = self.inline_rename.as_mut()
            && !rename.pending
        {
            rename.input.move_end(true);
            cx.notify();
        }
    }

    pub(crate) fn move_inline_rename_home(&mut self, cx: &mut Context<Self>) {
        if let Some(rename) = self.inline_rename.as_mut()
            && !rename.pending
        {
            rename.input.move_home(false);
            cx.notify();
        }
    }

    pub(crate) fn move_inline_rename_end(&mut self, cx: &mut Context<Self>) {
        if let Some(rename) = self.inline_rename.as_mut()
            && !rename.pending
        {
            rename.input.move_end(false);
            cx.notify();
        }
    }

    pub(crate) fn backspace_inline_rename(&mut self, cx: &mut Context<Self>) {
        self.delete_inline_rename_with_direction(true, cx);
    }

    pub(crate) fn delete_inline_rename(&mut self, cx: &mut Context<Self>) {
        self.delete_inline_rename_with_direction(false, cx);
    }

    fn delete_inline_rename_with_direction(&mut self, backwards: bool, cx: &mut Context<Self>) {
        let Some(rename) = self.inline_rename.as_mut() else {
            return;
        };
        if rename.pending {
            return;
        }
        rename.input.delete(backwards);
        cx.notify();
    }

    pub(crate) fn begin_inline_rename_from_selection(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.sidebar_keyboard_focus
            || self.sidebar_filter_focused
            || self.inline_rename.is_some()
        {
            return;
        }
        let selected = match self.sidebar_focus {
            SidebarFocus::Folder => self
                .selected_folder
                .clone()
                .map(|path| (path, InlineRenameKind::Folder)),
            SidebarFocus::ActiveSession => self
                .active_session()
                .path()
                .map(|path| (path.to_path_buf(), InlineRenameKind::File)),
        };
        let Some((path, kind)) = selected else {
            return;
        };
        self.begin_inline_rename(path, kind, window, cx);
    }

    pub(super) fn begin_inline_rename(
        &mut self,
        path: PathBuf,
        kind: InlineRenameKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.inline_rename.is_some()
            || self.work_folder.as_ref().is_none_or(|folder| match kind {
                InlineRenameKind::File => folder.entry_for_path(&path).is_none(),
                InlineRenameKind::Folder => folder.children_at(&path).is_none(),
            })
        {
            return;
        }
        if self.inline_rename_has_background_conflict(&path, kind) {
            self.status = Some("Rename deferred while another operation is in progress".to_owned());
            cx.notify();
            return;
        }
        let Some((text, fixed_extension)) = inline_rename_parts(&path, kind) else {
            self.status = Some("Rename failed: file or folder name is not valid UTF-8".to_owned());
            cx.notify();
            return;
        };
        let mut input = ShortTextInput::new(text);
        input.select_all();
        self.inline_rename = Some(InlineRename {
            kind,
            from: path,
            input,
            fixed_extension,
            pending: false,
        });
        window.focus(&self.focus_handle, cx);
        cx.notify();
    }

    pub(super) fn inline_rename_has_background_conflict(
        &self,
        from: &Path,
        kind: InlineRenameKind,
    ) -> bool {
        let belongs = |path: &Path| match kind {
            InlineRenameKind::File => path == from,
            InlineRenameKind::Folder => rebase_ui_path(path, from, from).is_some(),
        };
        self.folder_trash_blocks(from, true)
            || (kind == InlineRenameKind::Folder
                && self.pending_new_folders.iter().any(|path| belongs(path)))
            || self.sessions.sessions().any(|session| {
                session.path().is_some_and(belongs)
                    && (session.save_in_flight()
                        || self.title_sync_in_flight.contains(&session.id())
                        || self.title_sync_pending.contains_key(&session.id())
                        || (session.auto_title().is_some()
                            && self.title_sync_scheduled.contains_key(&session.id())))
            })
            || (kind == InlineRenameKind::Folder
                && self.work_folder_drafts.iter().any(|(id, draft)| {
                    belongs(&draft.target_directory)
                        && (self.title_sync_in_flight.contains(id)
                            || self.title_sync_pending.contains_key(id)
                            || self.title_sync_scheduled.contains_key(id))
                }))
    }

    pub(super) fn reserve_inline_rename_tickets(
        &mut self,
        from: &Path,
        kind: InlineRenameKind,
    ) -> Option<Vec<(SessionId, SaveTicket)>> {
        let ids: Vec<SessionId> = self
            .sessions
            .sessions()
            .filter(|session| {
                session.path().is_some_and(|path| match kind {
                    InlineRenameKind::File => path == from,
                    InlineRenameKind::Folder => rebase_ui_path(path, from, from).is_some(),
                })
            })
            .map(DocumentSession::id)
            .collect();
        if ids.iter().any(|id| {
            self.sessions
                .get(*id)
                .is_some_and(DocumentSession::save_in_flight)
        }) {
            return None;
        }
        let mut tickets = Vec::with_capacity(ids.len());
        // Publish protection before any save slot is taken (rename holds it),
        // then recompute from every session so nothing else is cleared.
        self.raise_external_open_guard();
        for id in ids {
            let Some(ticket) = self
                .sessions
                .get_mut(id)
                .and_then(DocumentSession::begin_rename)
            else {
                for (reserved_id, reserved_ticket) in tickets {
                    if let Some(session) = self.sessions.get_mut(reserved_id) {
                        session.finish_rename(reserved_ticket);
                    }
                }
                self.sync_external_open_guard();
                return None;
            };
            tickets.push((id, ticket));
        }
        self.sync_external_open_guard();
        Some(tickets)
    }

    pub(crate) fn confirm_inline_rename(&mut self, cx: &mut Context<Self>) {
        let Some(rename) = self.inline_rename.as_ref() else {
            return;
        };
        if rename.pending {
            return;
        }
        let from = rename.from.clone();
        let kind = rename.kind;
        let name = rename.input.text().to_owned();
        let fixed_extension = rename.fixed_extension.clone();
        if !valid_inline_rename_name(&name) {
            self.status = Some("Rename failed: invalid file or folder name".to_owned());
            cx.notify();
            return;
        }
        if self.loading_paths.iter().any(|path| match kind {
            InlineRenameKind::File => path == &from,
            InlineRenameKind::Folder => rebase_ui_path(path, &from, &from).is_some(),
        }) || self.inline_rename_has_background_conflict(&from, kind)
        {
            self.status = Some("Rename deferred while another operation is in progress".to_owned());
            cx.notify();
            return;
        }
        let Some(parent) = from.parent() else {
            self.status = Some("Rename failed: item has no parent directory".to_owned());
            cx.notify();
            return;
        };
        let file_name =
            fixed_extension.map_or_else(|| name.clone(), |extension| format!("{name}{extension}"));
        let target = parent.join(file_name);
        if target == from {
            self.inline_rename = None;
            cx.notify();
            return;
        }
        let Some(tickets) = self.reserve_inline_rename_tickets(&from, kind) else {
            self.status = Some("Rename deferred while a save is in progress".to_owned());
            cx.notify();
            return;
        };
        if let Some(rename) = self.inline_rename.as_mut() {
            rename.pending = true;
        }
        self.status = Some("Renaming…".to_owned());
        let files = self.files.clone();
        let operation_from = from.clone();
        let operation_target = target.clone();
        // The rename only ever changes the final path component (`target`
        // above is always `parent.join(file_name)`), so the containing
        // folder is the same before and after it lands; computed now, off
        // the tree the rename was requested against.
        let ancestor_folders = self
            .work_folder
            .as_ref()
            .map(|folder| folder.ancestor_folder_paths(&from))
            .unwrap_or_default();
        cx.spawn(async move |view, cx| {
            let (result, ancestors) = cx
                .background_executor()
                .spawn(async move {
                    let result = match kind {
                        InlineRenameKind::File => files.rename(&operation_from, &operation_target),
                        InlineRenameKind::Folder => {
                            files.rename_folder(&operation_from, &operation_target)
                        }
                    };
                    // The containing directory's own entry changed with the
                    // rename, so its real modified time is acquired here
                    // too. A failed rename never touched the filesystem, so
                    // nothing here changed either.
                    let ancestors = if result.is_ok() {
                        stamp_ancestor_folders(files.as_ref(), &ancestor_folders)
                    } else {
                        Vec::new()
                    };
                    (result, ancestors)
                })
                .await;
            let _ = view.update(cx, |view, cx| {
                let attempt = InlineRenameAttempt {
                    from,
                    target,
                    kind,
                    tickets,
                    ancestors,
                };
                view.finish_inline_rename(attempt, result, cx);
            });
        })
        .detach();
        cx.notify();
    }

    fn finish_inline_rename(
        &mut self,
        attempt: InlineRenameAttempt,
        result: std::io::Result<()>,
        cx: &mut Context<Self>,
    ) {
        let InlineRenameAttempt {
            from,
            target,
            kind,
            tickets,
            ancestors,
        } = attempt;
        let mut queued_saves = Vec::new();
        for (id, ticket) in tickets {
            if let Some(session) = self.sessions.get_mut(id) {
                session.finish_rename(ticket);
                if let Some(pending) = session.take_pending_save() {
                    queued_saves.push((id, pending));
                }
            }
        }
        match result {
            Ok(()) => {
                match kind {
                    InlineRenameKind::File => {
                        let outcomes = self.sessions.apply_file_event(&FileEvent::Renamed {
                            from: from.clone(),
                            to: target.clone(),
                        });
                        for (id, outcome) in outcomes {
                            if outcome == FileEventOutcome::Renamed
                                && let Some(session) = self.sessions.get_mut(id)
                            {
                                session.stop_auto_naming();
                            }
                        }
                        if let Some(folder) = self.work_folder.as_mut() {
                            folder.rename(&from, &target);
                        }
                        self.recent.rename(&from, &target);
                    }
                    InlineRenameKind::Folder => {
                        self.sessions.rename_folder(&from, &target);
                        if let Some(folder) = self.work_folder.as_mut() {
                            folder.rename_folder(&from, &target);
                        }
                        self.recent.rename_folder(&from, &target);
                    }
                }
                self.touch_work_folder_ancestors(&ancestors);
                self.follow_inline_rename_paths(&from, &target, kind);
                self.content_search_workspace_changed(cx);
                if let Err(error) = self.stores.recent_files().store(&self.recent) {
                    self.status = Some(format!("Recent files failed: {error}"));
                } else {
                    self.status = Some("Renamed".to_owned());
                }
                // After the status above, so a pin save warning is not
                // overwritten by it.
                self.follow_pin_rename(
                    &from,
                    &target,
                    match kind {
                        InlineRenameKind::File => hane_session::PinKind::File,
                        InlineRenameKind::Folder => hane_session::PinKind::Folder,
                    },
                    cx,
                );
                self.inline_rename = None;
            }
            Err(error) => {
                if let Some(rename) = self.inline_rename.as_mut() {
                    rename.pending = false;
                }
                self.status = Some(format!("Rename failed: {error}"));
            }
        }
        for (id, pending) in queued_saves {
            self.save_session(id, pending, cx);
        }
        self.retry_deferred_title_sync(cx);
        // Publish the exact state from every session before the next render.
        self.sync_external_open_guard();
        cx.notify();
    }

    fn follow_inline_rename_paths(&mut self, from: &Path, target: &Path, kind: InlineRenameKind) {
        let rebase = |path: &Path| match kind {
            InlineRenameKind::File => (path == from).then(|| target.to_path_buf()),
            InlineRenameKind::Folder => rebase_ui_path(path, from, target),
        };
        if kind == InlineRenameKind::Folder {
            self.selected_folder = self
                .selected_folder
                .take()
                .map(|path| rebase(&path).unwrap_or(path));
            let expanded = std::mem::take(&mut self.expanded_folders);
            self.expanded_folders = expanded
                .into_iter()
                .map(|path| rebase(&path).unwrap_or(path))
                .collect();
            let pending = std::mem::take(&mut self.pending_new_folders);
            self.pending_new_folders = pending
                .into_iter()
                .map(|path| rebase(&path).unwrap_or(path))
                .collect();
            for draft in self.work_folder_drafts.values_mut() {
                if let Some(path) = rebase(&draft.target_directory) {
                    draft.target_directory = path;
                }
            }
        }
        let loading = std::mem::take(&mut self.loading_paths);
        self.loading_paths = loading
            .into_iter()
            .map(|path| rebase(&path).unwrap_or(path))
            .collect();
        self.latest_open_target = self
            .latest_open_target
            .take()
            .map(|path| rebase(&path).unwrap_or(path));
    }

    /// Cancels an editable rename and reports whether the caller may proceed
    /// with the navigation/action that requested the cancellation. A pending
    /// filesystem rename cannot be cancelled safely, so callers must stop.
    pub(crate) fn cancel_inline_rename(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(rename) = self.inline_rename.as_ref() else {
            return true;
        };
        if rename.pending {
            self.status = Some("Rename in progress".to_owned());
            cx.notify();
            return false;
        }
        self.inline_rename = None;
        self.retry_deferred_title_sync(cx);
        cx.notify();
        true
    }
}
