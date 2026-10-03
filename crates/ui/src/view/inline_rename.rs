use super::*;

impl EditorView {
    pub(crate) fn inline_rename_active(&self) -> bool {
        self.inline_rename.is_some()
    }

    pub(crate) fn inline_rename_render_state(&self) -> Option<InlineRenameRenderState> {
        self.inline_rename
            .as_ref()
            .map(|rename| InlineRenameRenderState {
                text: rename.text.clone(),
                selected_range: rename.selected_range.clone(),
                selection_reversed: rename.selection_reversed,
                marked_range: rename.marked_range.clone(),
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
        if rename.pending || rename.composition.is_some() {
            return;
        }
        let byte = byte_offset_from_utf16(&rename.text, index);
        rename.selected_range = byte..byte;
        rename.selection_reversed = false;
        cx.notify();
    }

    pub(crate) fn inline_rename_has_composition(&self) -> bool {
        self.inline_rename
            .as_ref()
            .is_some_and(|rename| rename.composition.is_some())
    }

    pub(crate) fn inline_rename_text_for_range(
        &self,
        range_utf16: Range<usize>,
    ) -> Option<(String, Range<usize>)> {
        let rename = self.inline_rename.as_ref()?;
        let range = byte_range_from_utf16(&rename.text, &range_utf16);
        Some((
            rename.text[range.clone()].to_owned(),
            range_to_utf16(&rename.text, &range),
        ))
    }

    pub(crate) fn inline_rename_selection(&self) -> Option<(Range<usize>, bool)> {
        self.inline_rename.as_ref().map(|rename| {
            (
                range_to_utf16(&rename.text, &rename.selected_range),
                rename.selection_reversed,
            )
        })
    }

    pub(crate) fn inline_rename_marked_range(&self) -> Option<Range<usize>> {
        let rename = self.inline_rename.as_ref()?;
        rename
            .marked_range
            .as_ref()
            .map(|range| range_to_utf16(&rename.text, range))
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
        let range = range_utf16
            .as_ref()
            .map(|range| byte_range_from_utf16(&rename.text, range))
            .or_else(|| rename.marked_range.clone())
            .unwrap_or_else(|| rename.selected_range.clone());
        let replacement: String = new_text
            .chars()
            .filter(|character| *character != '\n' && *character != '\r')
            .collect();
        rename.text.replace_range(range.clone(), &replacement);
        let next = range.start + replacement.len();
        rename.selected_range = next..next;
        rename.selection_reversed = false;
        rename.marked_range = None;
        rename.composition = None;
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
        if rename.composition.is_none() {
            rename.composition = Some(InlineRenameComposition {
                text: rename.text.clone(),
                selected_range: rename.selected_range.clone(),
                selection_reversed: rename.selection_reversed,
            });
        }
        let range = range_utf16
            .as_ref()
            .map(|range| byte_range_from_utf16(&rename.text, range))
            .or_else(|| rename.marked_range.clone())
            .unwrap_or_else(|| rename.selected_range.clone());
        let replacement: String = new_text
            .chars()
            .filter(|character| *character != '\n' && *character != '\r')
            .collect();
        rename.text.replace_range(range.clone(), &replacement);
        let marked_end = range.start + replacement.len();
        rename.marked_range = (!replacement.is_empty()).then_some(range.start..marked_end);
        rename.selected_range = inline_rename_selected_range(
            range.start,
            &replacement,
            new_selected_range_utf16,
            marked_end..marked_end,
        );
        rename.selection_reversed = false;
        cx.notify();
        true
    }

    pub(crate) fn commit_inline_rename_composition(&mut self, cx: &mut Context<Self>) {
        let Some(rename) = self.inline_rename.as_mut() else {
            return;
        };
        if rename.composition.take().is_some() {
            rename.marked_range = None;
            cx.notify();
        }
    }

    pub(crate) fn cancel_inline_rename_composition(&mut self, cx: &mut Context<Self>) {
        let Some(rename) = self.inline_rename.as_mut() else {
            return;
        };
        let Some(composition) = rename.composition.take() else {
            return;
        };
        rename.text = composition.text;
        rename.selected_range = composition.selected_range;
        rename.selection_reversed = composition.selection_reversed;
        rename.marked_range = None;
        cx.notify();
    }

    pub(crate) fn selected_inline_rename_text(&self) -> Option<String> {
        let rename = self.inline_rename.as_ref()?;
        (!rename.selected_range.is_empty())
            .then(|| rename.text[rename.selected_range.clone()].to_owned())
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
        let cursor = if rename.selection_reversed {
            rename.selected_range.start
        } else {
            rename.selected_range.end
        };
        let target = if right {
            next_inline_rename_boundary(&rename.text, cursor)
        } else {
            previous_inline_rename_boundary(&rename.text, cursor)
        };
        if extend {
            select_inline_rename_to(rename, target);
        } else if rename.selected_range.is_empty() {
            rename.selected_range = target..target;
            rename.selection_reversed = false;
        } else {
            let target = if right {
                rename.selected_range.end
            } else {
                rename.selected_range.start
            };
            rename.selected_range = target..target;
            rename.selection_reversed = false;
        }
        cx.notify();
    }

    pub(crate) fn select_inline_rename_left(&mut self, cx: &mut Context<Self>) {
        let target = self.inline_rename.as_ref().map(|rename| {
            previous_inline_rename_boundary(&rename.text, inline_rename_cursor(rename))
        });
        if let Some(target) = target {
            if let Some(rename) = self.inline_rename.as_mut() {
                select_inline_rename_to(rename, target);
            }
            cx.notify();
        }
    }

    pub(crate) fn select_inline_rename_right(&mut self, cx: &mut Context<Self>) {
        let target = self
            .inline_rename
            .as_ref()
            .map(|rename| next_inline_rename_boundary(&rename.text, inline_rename_cursor(rename)));
        if let Some(target) = target {
            if let Some(rename) = self.inline_rename.as_mut() {
                select_inline_rename_to(rename, target);
            }
            cx.notify();
        }
    }

    pub(crate) fn select_all_inline_rename(&mut self, cx: &mut Context<Self>) {
        if let Some(rename) = self.inline_rename.as_mut()
            && !rename.pending
        {
            rename.selected_range = 0..rename.text.len();
            rename.selection_reversed = false;
            cx.notify();
        }
    }

    pub(crate) fn select_inline_rename_home(&mut self, cx: &mut Context<Self>) {
        if let Some(rename) = self.inline_rename.as_mut()
            && !rename.pending
        {
            select_inline_rename_to(rename, 0);
            cx.notify();
        }
    }

    pub(crate) fn select_inline_rename_end(&mut self, cx: &mut Context<Self>) {
        let target = self
            .inline_rename
            .as_ref()
            .map_or(0, |rename| rename.text.len());
        if let Some(rename) = self.inline_rename.as_mut()
            && !rename.pending
        {
            select_inline_rename_to(rename, target);
            cx.notify();
        }
    }

    pub(crate) fn move_inline_rename_home(&mut self, cx: &mut Context<Self>) {
        self.move_inline_rename_to(0, cx);
    }

    pub(crate) fn move_inline_rename_end(&mut self, cx: &mut Context<Self>) {
        let target = self
            .inline_rename
            .as_ref()
            .map_or(0, |rename| rename.text.len());
        self.move_inline_rename_to(target, cx);
    }

    fn move_inline_rename_to(&mut self, target: usize, cx: &mut Context<Self>) {
        if let Some(rename) = self.inline_rename.as_mut()
            && !rename.pending
        {
            rename.selected_range = target..target;
            rename.selection_reversed = false;
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
        let range = if rename.selected_range.is_empty() {
            let cursor = inline_rename_cursor(rename);
            if backwards {
                previous_inline_rename_boundary(&rename.text, cursor)..cursor
            } else {
                cursor..next_inline_rename_boundary(&rename.text, cursor)
            }
        } else {
            rename.selected_range.clone()
        };
        rename.text.replace_range(range.clone(), "");
        rename.selected_range = range.start..range.start;
        rename.selection_reversed = false;
        rename.marked_range = None;
        rename.composition = None;
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
        let text_len = text.len();
        self.inline_rename = Some(InlineRename {
            kind,
            from: path,
            text,
            fixed_extension,
            selected_range: 0..text_len,
            selection_reversed: false,
            marked_range: None,
            composition: None,
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
        (kind == InlineRenameKind::Folder
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

    fn reserve_inline_rename_tickets(
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
                return None;
            };
            tickets.push((id, ticket));
        }
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
        let name = rename.text.clone();
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
        cx.spawn(async move |view, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    match kind {
                        InlineRenameKind::File => files.rename(&operation_from, &operation_target),
                        InlineRenameKind::Folder => {
                            files.rename_folder(&operation_from, &operation_target)
                        }
                    }
                })
                .await;
            let _ = view.update(cx, |view, cx| {
                view.finish_inline_rename(from, target, kind, tickets, result, cx);
            });
        })
        .detach();
        cx.notify();
    }

    fn finish_inline_rename(
        &mut self,
        from: PathBuf,
        target: PathBuf,
        kind: InlineRenameKind,
        tickets: Vec<(SessionId, SaveTicket)>,
        result: std::io::Result<()>,
        cx: &mut Context<Self>,
    ) {
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
                self.follow_inline_rename_paths(&from, &target, kind);
                self.content_search_workspace_changed(cx);
                if let Err(error) = self.stores.recent_files().store(&self.recent) {
                    self.status = Some(format!("Recent files failed: {error}"));
                } else {
                    self.status = Some("Renamed".to_owned());
                }
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
