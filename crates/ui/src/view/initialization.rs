use super::*;

impl EditorView {
    pub fn new(text: &str, file_label: impl Into<String>, cx: &mut Context<Self>) -> Self {
        Self::from_sessions(
            SessionSet::with_untitled(text, file_label),
            Arc::new(OsFileService),
            StateStores::from_environment(),
            cx,
        )
    }

    pub(super) fn from_sessions(
        sessions: SessionSet,
        files: Arc<dyn FileService>,
        stores: StateStores,
        cx: &mut Context<Self>,
    ) -> Self {
        crate::init_components(cx);
        let theme = DEFAULT_THEME;
        let settings = stores.settings().load();
        let recent = stores.recent_files().load();
        let heights = HeightIndex::new(std::iter::repeat_n(
            theme.line_height,
            sessions.active().editor().document().line_count(),
        ));
        // The 750ms draft-save debounce (`schedule_draft_save`) can still be
        // pending when the app quits; this hook flushes whatever it would
        // have written so an unnamed note typed just before quitting is not
        // lost.
        let quit_subscription = cx.on_app_quit(|view: &mut Self, _cx| {
            view.flush_pending_drafts();
            std::future::ready(())
        });
        // The diagrams' pictures are also held by the window's atlas; a view
        // that goes away returns them so that memory is freed with it.
        let mermaid_release_subscription = cx.on_release(|view: &mut Self, app| {
            for retired in view.mermaid.release_all() {
                let (image, _reservation) = retired.into_parts();
                app.drop_image(image, None);
            }
        });
        // The caret's mode badge (see `caret_input_mode`) has to
        // update the moment the user switches IME mode, even if nothing else
        // about the document changes, so this reuses the same platform event
        // GPUI already refreshes its keyboard mapper from — no separate
        // polling.
        let input_mode_view = cx.entity().downgrade();
        let input_mode_subscription = cx.on_keyboard_layout_change(move |app| {
            let _ = input_mode_view.update(app, |view, cx| {
                view.refresh_caret_input_mode_for_keyboard_layout_change(cx);
            });
        });
        // The pinned find-input widget unmarks its own active IME composition
        // on Escape and then still lets the keystroke bubble out to this
        // view's own `CancelComposition` handler (see
        // `note_document_find_escape_keystroke`), so by the time that handler
        // runs, the composition it needs to distinguish is already gone. An
        // interceptor runs before any binding for the keystroke is dispatched
        // (unlike a normal action or key-down listener), so it is the only
        // point that still observes the find input's marked range as it was
        // just before this Escape.
        let escape_interceptor_view = cx.entity().downgrade();
        let document_find_escape_interceptor =
            cx.intercept_keystrokes(move |event, window, app| {
                if event.keystroke.key != "escape" {
                    return;
                }
                let _ = escape_interceptor_view.update(app, |view, cx| {
                    view.note_document_find_escape_keystroke(window, cx);
                    view.note_ai_selection_escape_keystroke(window, cx);
                });
            });
        // The pinned find-input widget's `InputState::enter` emits
        // `InputEvent::PressEnter` for every Enter — including one that only
        // confirms an in-progress IME composition — without checking its own
        // marked range first. This interceptor runs before that (or any
        // other) binding for the keystroke is dispatched, so it is the only
        // point that still observes the find input's marked range as it was
        // just before this Enter, the same reasoning as the Escape
        // interceptor above (see `note_document_find_enter_keystroke`).
        let enter_interceptor_view = cx.entity().downgrade();
        let inline_rename_swallowed_input = std::rc::Rc::new(std::cell::Cell::new(false));
        // A new keystroke ends the window in which text the platform already
        // received during a pending rename could still be delivered late (see
        // `drop_input_after_pending_rename`). A shared cell avoids updating the
        // view from inside key dispatch.
        let swallowed_input_for_interceptor = inline_rename_swallowed_input.clone();
        let document_find_enter_interceptor = cx.intercept_keystrokes(move |event, window, app| {
            // A key pressed while the rename is pending arms the guard right
            // away: its text callback may only arrive after the rename ended,
            // so waiting for a callback during the pending window is not enough.
            let rename_pending = enter_interceptor_view
                .read_with(&*app, |view, _| {
                    view.inline_rename
                        .as_ref()
                        .is_some_and(|rename| rename.pending)
                })
                .unwrap_or(false);
            // An Enter may only confirm an IME composition whose text callback
            // is still in flight, so it neither arms nor clears the guard; any
            // other key is new input and ends the guarded window.
            let is_enter = event.keystroke.key == "enter";
            if rename_pending {
                swallowed_input_for_interceptor.set(true);
            } else if !is_enter {
                swallowed_input_for_interceptor.set(false);
            }
            if !is_enter {
                return;
            }
            let _ = enter_interceptor_view.update(app, |view, cx| {
                view.note_document_find_enter_keystroke(window, cx);
                view.note_ai_selection_enter_keystroke(window, cx);
            });
        });
        // Re-observes the local date on a timer so the sidebar's `本日`
        // badge moves on even when the window sits open, focused, and
        // untouched across local midnight; `view.update` failing (the view
        // has been dropped) ends the loop instead of polling forever.
        let date_badge_refresh_task = cx.spawn(async move |view, cx| {
            loop {
                cx.background_executor()
                    .timer(DATE_BADGE_REFRESH_INTERVAL)
                    .await;
                if view
                    .update(cx, |view, cx| view.refresh_sidebar_date_badge_today(cx))
                    .is_err()
                {
                    break;
                }
            }
        });
        Self {
            sessions,
            files,
            stores,
            settings,
            settings_open: false,
            settings_category: SettingsCategory::General,
            settings_focus_handle: cx.focus_handle(),
            ai_settings: ai_settings::AiSettingsPage::default(),
            ai_prompts: ai_prompts::AiPromptsPage::default(),
            network_settings: network_settings::NetworkSettingsPage::default(),
            network_applier: None,
            ai_selection:ai_selection::AiSelectionState::default(),
            app_update: None,
            app_update_polling: false,
            restart_freeze: false,
            external_open_generation: 0,
            external_open_inbox: None,
            external_open_guard: ExternalOpenGuard::new(),
            save_as_dialogs_open: 0,
            restart_ai_service: None,
            #[cfg(test)]
            restart_test_ai_busy: false,
            ai_lifecycle: None,
            restart_in_progress: false,
            restart_message: None,
            update_completed: None,
            explorer_migration_failed: None,
            file_context_menu_state: FileContextMenuState::NotChecked,
            file_context_menu_busy: false,
            file_context_menu_generation: 0,
            pending_settings_drop: None,
            pending_settings_drop_protected: false,
            settings_error: None,
            recent,
            work_folder: None,
            content_search: content_search::ContentSearchState::default(),
            document_find: document_find::DocumentFindState::default(),
            draft_store: Arc::new(OsDraftStore),
            work_folder_drafts: HashMap::new(),
            selected_folder: None,
            sidebar_focus: SidebarFocus::ActiveSession,
            expanded_folders: HashSet::new(),
            sidebar_filter: ShortTextInput::default(),
            sidebar_filter_focused: false,
            sidebar_filter_input_bounds: None,
            sidebar_keyboard_focus: false,
            inline_rename: None,
            inline_rename_swallowed_input,
            sidebar_width: theme.sidebar_width,
            sidebar_collapsed: false,
            sidebar_resize_drag: None,
            sidebar_scroll: ScrollHandle::new(),
            sidebar_pins: sidebar_pins::SidebarPins::default(),
            file_tabs_scroll: ScrollHandle::new(),
            file_tab_context_menu: None,
            sidebar_context_menu: None,
            editor_context_menu: None,
            editor_context_menu_generation: 0,
            editor_context_menu_activation: None,
            tab_close_confirm: None,
            tab_close_after_save: HashMap::new(),
            save_problems: HashMap::new(),
            settings_store_error: None,
            sidebar_scrollbar_drag: None,
            editor_scrollbar_drag: None,
            text_selection_drag: false,
            pending_reading_link: None,
            text_autoscroll: None,
            text_autoscroll_activity: 0,
            sidebar_scrollbar_visible: false,
            sidebar_scrollbar_activity: 0,
            sidebar_date_badge_today: local_today(),
            _date_badge_refresh_task: date_badge_refresh_task,
            pending_new_folders: HashSet::new(),
            folder_trash_in_flight: None,
            inline_rename_input_bounds: None,
            _quit_subscription: quit_subscription,
            editor_mode: EditorMode::default(),
            caret_input_mode: active_keyboard_input_mode(),
            _input_mode_subscription: input_mode_subscription,
            _input_mode_focus_subscription: None,
            _document_find_escape_interceptor: document_find_escape_interceptor,
            _document_find_enter_interceptor: document_find_enter_interceptor,
            draft_recovery_warning: None,
            title_sync_pending: HashMap::new(),
            title_sync_in_flight: HashSet::new(),
            title_sync_scheduled: HashMap::new(),
            title_sync_deferred: HashSet::new(),
            loading_paths: HashSet::new(),
            latest_open_target: None,
            pending_restore: VecDeque::new(),
            restoring_path: None,
            work_folder_generation: 0,
            external_folder_open_token: 0,
            external_file_open_after_folder_scan: None,
            folder_watch: folder_watch::FolderWatchState::default(),
            listing_requests: folder_watch::ListingRequests::default(),
            focus_handle: cx.focus_handle(),
            heights,
            scroll_y: 0.0,
            viewport_height: theme.line_height,
            metrics: FrameMetrics::new(METRICS_CAPACITY),
            status: None,
            theme,
            #[cfg(any(feature = "instrument", feature = "timing-probe"))]
            instrumentation: Instrumentation::from_environment(),
            background_presentation_generation: 0,
            block_cache: HashMap::new(),
            line_owners: HashMap::new(),
            layout_cache: HashMap::new(),
            content_width: 0.0,
            main_column_left: 0.0,
            layout_font_revision: 0,
            zoom: 1.0,
            raw_zoom: 1.0,
            wheel_zoom_animation: None,
            scroll_inertia: None,
            pending_zoom_anchor: None,
            pending_mode_anchor: None,
            mode_generation: 0,
            caret_geometry: None,
            pending_caret_visibility_after_layout: false,
            layout_followup_frames_requested: 0,
            pending_center_caret_frames: 0,
            center_window_line: None,
            retained_center_window: None,
            pending_find_visibility_after_layout: false,
            block_index: BlockIndexState::new(),
            granularity: Granularity::Lines,
            height_blocks: HeightBlocks::default(),
            document_parse_job_running: false,
            last_background_height_disclosure: None,
            last_applied_height_disclosure: None,
            last_endpoint_height_disclosure: None,
            force_height_disclosure_snapshot: false,
            joined_parse_cache: HashMap::new(),
            joined_parse_jobs: HashMap::new(),
            joined_parse_jobs_running: 0,
            mermaid: MermaidStore::new(),
            _mermaid_release_subscription: mermaid_release_subscription,
        }
    }
}
