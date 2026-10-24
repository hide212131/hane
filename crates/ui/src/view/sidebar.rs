use super::*;
use gpui_component::Icon;

/// One row of the flattened sidebar tree: a node together with how deeply it
/// is nested, so indentation can be applied without recursive rendering.
pub(super) struct WorkFolderRow<'a> {
    depth: usize,
    pub(super) node: &'a WorkFolderNode,
}

/// Flattens the tree into display order (depth-first, each level already
/// sorted by `WorkFolder`/`WorkFolderFolder`), descending into a folder only
/// when it is in `expanded`. The work folder root itself is always expanded.
pub(super) fn flatten_work_folder_tree<'a>(
    nodes: &'a [WorkFolderNode],
    depth: usize,
    expanded: &HashSet<PathBuf>,
    out: &mut Vec<WorkFolderRow<'a>>,
) {
    for node in nodes {
        out.push(WorkFolderRow { depth, node });
        if let WorkFolderNode::Folder(folder) = node
            && expanded.contains(folder.path())
        {
            flatten_work_folder_tree(folder.children(), depth + 1, expanded, out);
        }
    }
}

/// Builds the rows shown while the sidebar filter is non-empty. A folder is
/// retained only when one of its descendants matches; retained folders are
/// always descended into so a match is reachable regardless of the user's
/// normal expansion state. The original `expanded_folders` is never touched.
pub(super) fn flatten_filtered_work_folder_tree<'a>(
    nodes: &'a [WorkFolderNode],
    depth: usize,
    query: &str,
    out: &mut Vec<WorkFolderRow<'a>>,
) -> bool {
    let mut found = false;
    for node in nodes {
        match node {
            WorkFolderNode::File(entry) => {
                if entry.file_name().to_lowercase().contains(query) {
                    out.push(WorkFolderRow { depth, node });
                    found = true;
                }
            }
            WorkFolderNode::Folder(folder) => {
                let mut descendants = Vec::new();
                if flatten_filtered_work_folder_tree(
                    folder.children(),
                    depth + 1,
                    query,
                    &mut descendants,
                ) {
                    out.push(WorkFolderRow { depth, node });
                    out.extend(descendants);
                    found = true;
                }
            }
        }
    }
    found
}

/// A short label for the sidebar toolbar's sort button. The full
/// `WorkFolderSortOrder::label()` text (shown in the dropdown menu and on
/// the settings screen, which both have room to spare) does not fit next to
/// the toolbar's other controls at `SIDEBAR_MIN_WIDTH`, so the toolbar shows
/// this shorter form and carries the full text via the button's tooltip and
/// accessibility label instead.
fn sidebar_sort_toolbar_label(order: WorkFolderSortOrder) -> &'static str {
    match order {
        WorkFolderSortOrder::UpdatedNewest => "新しい順",
        WorkFolderSortOrder::UpdatedOldest => "古い順",
        WorkFolderSortOrder::NameAscending => "名前（昇順）",
        WorkFolderSortOrder::NameDescending => "名前（降順）",
    }
}

impl EditorView {
    /// Re-observes the local calendar date for the sidebar's file-name
    /// badges and redraws the view when it has moved on. Driven by
    /// `_date_badge_refresh_task` so a window left open, focused, and
    /// untouched across local midnight still shows `本日` move to the new
    /// day, rather than only refreshing on the next unrelated redraw.
    pub(super) fn refresh_sidebar_date_badge_today(&mut self, cx: &mut Context<Self>) {
        self.apply_sidebar_date_badge_today(local_today(), cx);
    }

    /// The state update `refresh_sidebar_date_badge_today` drives, split out
    /// so the "did today actually change" decision is unit-testable without
    /// depending on the system clock or a real timer. Returns whether
    /// `today` differed from what the sidebar last used; `cx.notify()` only
    /// fires in that case, so a recheck that lands on the same day is a
    /// no-op redraw-wise.
    pub(super) fn apply_sidebar_date_badge_today(
        &mut self,
        today: CalendarDate,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.sidebar_date_badge_today == today {
            return false;
        }
        self.sidebar_date_badge_today = today;
        cx.notify();
        true
    }

    fn inline_rename_label(&self, cx: &mut Context<Self>) -> gpui::Div {
        let Some(rename) = self.inline_rename.as_ref() else {
            return div();
        };
        let input = div()
            .id("inline-rename-input")
            .flex_1()
            .min_w(px(0.0))
            .h_full()
            .flex()
            .items_center()
            .child(InlineRenameInput { input: cx.entity() });
        let mut label = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_1()
            .flex_1()
            .min_w(px(0.0))
            .child(input);
        if let Some(extension) = rename.fixed_extension.as_ref() {
            label = label.child(div().flex_none().child(extension.clone()));
        }
        label
    }

    /// Whether there is a side panel to show at all: a work folder is open or
    /// the content-search panel is active. A lone file has no panel, so the
    /// toggle is a no-op there instead of creating an empty one.
    pub(super) fn sidebar_panel_available(&self) -> bool {
        self.work_folder.is_some() || self.content_search_sidebar_visible()
    }

    /// Whether the panel is drawn now. Render, width, `main_column_left`,
    /// `content_width` and the footer width all read this one answer.
    pub(super) fn sidebar_shown(&self) -> bool {
        self.sidebar_panel_available() && !self.sidebar_collapsed
    }

    /// Whether the toggle must leave everything as it is: an IME composition
    /// or a rename (running, or waiting for its result) would be lost or
    /// confused by moving focus or hiding the field.
    fn sidebar_toggle_blocked(&self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        self.settings_open
            || self.ai_selection_blocks_editor_keys()
            || self.tab_close_confirm.is_some()
            || self.inline_rename_active()
            || self.editor().ime().is_some()
            || self.sidebar_filter_has_composition()
            || self.content_search_input_is_composing(window, cx)
            || self.document_find_input_is_composing(window, cx)
    }

    /// Opens or closes the left side panel from the button or Command/Ctrl+B.
    /// Only visibility changes: the work folder, search state, widths,
    /// scrolls and the document are untouched, and nothing is scanned,
    /// searched, loaded or saved.
    pub(crate) fn toggle_sidebar(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Checked before any blur, focus move or visibility change.
        if self.sidebar_toggle_blocked(window, cx) || !self.sidebar_panel_available() {
            return;
        }
        if self.sidebar_collapsed {
            self.sidebar_collapsed = false;
            cx.notify();
            return;
        }
        let hidden_input_focused = self.sidebar_filter_focused
            || self.content_search_input_is_focused()
            || self.content_search_results_focused();
        self.sidebar_collapsed = true;
        self.sidebar_resize_drag = None;
        self.sidebar_scrollbar_drag = None;
        self.sidebar_context_menu = None;
        self.blur_sidebar_filter(cx);
        self.blur_content_search_focus(cx);
        self.sidebar_keyboard_focus = false;
        if hidden_input_focused {
            // The hidden filter/search input may hold the real GPUI focus;
            // hand it back to the body so typing reaches the document.
            window.focus(&self.focus_handle, cx);
        }
        cx.notify();
    }

    /// The shared open/close button: 32px square with a 20px icon. The
    /// tooltip names the action the current state would perform and its key.
    fn sidebar_toggle_button(&self, cx: &mut Context<Self>) -> gpui::Div {
        let label = if self.sidebar_shown() {
            "サイドパネルを閉じる"
        } else {
            "サイドパネルを開く"
        };
        let key = if cfg!(target_os = "macos") {
            "⌘B"
        } else {
            "Ctrl+B"
        };
        let view = cx.entity();
        div()
            .debug_selector(|| "sidebar-toggle".to_owned())
            .w(px(SIDEBAR_TOGGLE_BUTTON_SIZE))
            .h(px(SIDEBAR_TOGGLE_BUTTON_SIZE))
            .flex_none()
            .child(
                Button::new("sidebar-toggle")
                    .ghost()
                    .w_full()
                    .h_full()
                    .tooltip(format!("{label} ({key})"))
                    .accessibility_label(label)
                    // A bare `svg()` child is laid out by the Button's own
                    // content flex and rendered as an I-shaped glyph; the
                    // `Icon` path (as in the reading-mode toggle) sizes the
                    // 24px viewBox glyph correctly.
                    .icon(
                        Icon::empty()
                            .path(icons::ICON_SIDEBAR)
                            .size(px(SIDEBAR_TOGGLE_ICON_SIZE))
                            .text_color(rgb(self.theme.sidebar_foreground)),
                    )
                    .on_click(move |_, window, app| {
                        view.update(app, |view, cx| view.toggle_sidebar(window, cx));
                    }),
            )
    }

    /// The toggle shown in the editor header while the panel is hidden. It
    /// is `None` when there is no panel to open or the panel shows its own.
    pub(super) fn sidebar_toggle_in_header(&self, cx: &mut Context<Self>) -> Option<gpui::Div> {
        (self.sidebar_panel_available() && self.sidebar_collapsed).then(|| {
            // Fill the header and pad from its top edge, so the button sits at
            // the same y as in the panel row whether or not the header's
            // bottom border shrinks the centered area.
            div()
                .flex_none()
                .h_full()
                .ml(px(SIDEBAR_TOGGLE_LEFT))
                .pt(px(
                    (SIDEBAR_TOGGLE_ROW_HEIGHT - SIDEBAR_TOGGLE_BUTTON_SIZE) / 2.0
                ))
                .child(self.sidebar_toggle_button(cx))
        })
    }

    /// The left side panel with its fixed 40px toggle row on top. The
    /// existing panel gets the remaining height, so its internal list and
    /// scrollbar coordinates stay local to it.
    pub(super) fn work_folder_sidebar(
        &self,
        sidebar_viewport_height: f32,
        cx: &mut Context<Self>,
    ) -> Option<gpui::Stateful<gpui::Div>> {
        if !self.sidebar_shown() {
            return None;
        }
        let panel_height = (sidebar_viewport_height - SIDEBAR_TOGGLE_ROW_HEIGHT).max(0.0);
        let panel = if self.content_search_sidebar_visible() {
            self.content_search_sidebar(panel_height, cx)
        } else {
            self.work_folder_files_sidebar(panel_height, cx)?
        };
        let toggle_row = div()
            .id("sidebar-toggle-row")
            .h(px(SIDEBAR_TOGGLE_ROW_HEIGHT))
            .flex_none()
            .flex()
            .flex_row()
            .items_center()
            .pl(px(SIDEBAR_TOGGLE_LEFT))
            .child(self.sidebar_toggle_button(cx));
        Some(
            div()
                .id("work-folder-column")
                .flex_none()
                .w(px(self.sidebar_width))
                .h_full()
                .flex()
                .flex_col()
                .overflow_hidden()
                .bg(rgb(self.theme.sidebar_background))
                .child(toggle_row)
                .child(
                    div()
                        .id("work-folder-panel-frame")
                        .flex_1()
                        .min_h(px(0.0))
                        .overflow_hidden()
                        .child(panel),
                ),
        )
    }

    /// The folder/file tree for the sidebar, when this window was opened onto
    /// a work folder. File and folder rows reserve the same disclosure slot,
    /// so their file/folder icons line up and a folder does not shift when its
    /// disclosure icon changes direction.
    fn work_folder_files_sidebar(
        &self,
        sidebar_viewport_height: f32,
        cx: &mut Context<Self>,
    ) -> Option<gpui::Stateful<gpui::Div>> {
        let work_folder = self.work_folder.as_ref()?;
        let active_id = self.sessions.active_id();
        let active_path = self.sessions.active().path();
        let row_icon = |path: &'static str, color: u32| {
            gpui::svg()
                .path(path)
                .flex_none()
                .size_4()
                .text_color(rgb(color))
        };
        // The leading slot of a row: normally the folder/file icon, sized so
        // file and folder names start at the same x position. For folders,
        // hovering the icon swaps it for the expand/collapse chevron in
        // place, rather than reserving a separate chevron column up front.
        let entry_icon =
            |icon_path: &'static str, disclosure_path: Option<&'static str>, color: u32| {
                let icon_slot = div().relative().flex_none().size_4();
                match disclosure_path {
                    None => icon_slot.child(row_icon(icon_path, color)),
                    Some(disclosure_path) => icon_slot
                        .group("work-folder-row-icon")
                        .child(
                            div()
                                .group_hover("work-folder-row-icon", |style| style.invisible())
                                .child(row_icon(icon_path, color)),
                        )
                        .child(
                            div()
                                .absolute()
                                .inset_0()
                                .invisible()
                                .flex()
                                .items_center()
                                .justify_center()
                                .group_hover("work-folder-row-icon", |style| style.visible())
                                .child(
                                    gpui::svg()
                                        .path(disclosure_path)
                                        .flex_none()
                                        .w(px(12.0))
                                        .h(px(12.0))
                                        .text_color(rgb(color)),
                                ),
                        ),
                }
            };
        let toolbar_button = |id: &'static str, icon_path: &'static str| {
            div()
                .id(id)
                .w(px(24.0))
                .h(px(24.0))
                .flex()
                .items_center()
                .justify_center()
                .rounded_sm()
                .cursor_pointer()
                .bg(rgb(self.theme.code_background))
                .child(row_icon(icon_path, self.theme.foreground))
        };
        let sidebar_sort = self.settings.sidebar_sort;
        let sort_menu_view = cx.entity();
        let sort_button = Button::new("work-folder-sort-menu")
            .label(sidebar_sort_toolbar_label(sidebar_sort))
            .tooltip(format!("並べ替え: {}", sidebar_sort.label()))
            .accessibility_label(format!("並べ替え: {}", sidebar_sort.label()))
            .small()
            .ghost()
            .dropdown_menu(move |mut menu, _, _| {
                for order in WorkFolderSortOrder::all() {
                    let view = sort_menu_view.clone();
                    menu = menu.item(
                        PopupMenuItem::element(move |_, _| {
                            div()
                                .id(order.as_str())
                                .debug_selector(move || {
                                    format!("sidebar-sort-item-{}", order.as_str())
                                })
                                .px(px(5.0))
                                .py(px(3.0))
                                .child(order.label())
                        })
                        .checked(order == sidebar_sort)
                        .on_click(move |_, _window, app| {
                            view.update(app, |view, cx| {
                                view.set_sidebar_sort(order, cx);
                            });
                        }),
                    );
                }
                menu
            });
        let sort_button = div()
            .id("work-folder-sort-menu-wrapper")
            .debug_selector(|| "sidebar-sort-menu".to_owned())
            .child(sort_button);
        let toolbar = div()
            .id("work-folder-toolbar")
            .debug_selector(|| "sidebar-toolbar".to_owned())
            .h(px(SIDEBAR_TOOLBAR_HEIGHT))
            .flex_none()
            .flex()
            .flex_row()
            .items_center()
            .gap_1()
            .mb(px(SIDEBAR_TOOLBAR_GAP))
            .child(
                toolbar_button("work-folder-new-note", icons::ICON_FILE_NEW)
                    .on_click(cx.listener(|view, _, _, cx| view.new_work_folder_note(cx))),
            )
            .child(
                toolbar_button("work-folder-new-folder", icons::ICON_FOLDER_NEW)
                    .on_click(cx.listener(|view, _, _, cx| view.new_work_folder_folder(cx))),
            )
            .child(
                div()
                    .id("work-folder-content-search")
                    .h(px(24.0))
                    .px_1()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_sm()
                    .cursor_pointer()
                    .bg(rgb(self.theme.code_background))
                    .child("本文検索")
                    .on_click(cx.listener(|view, _, window, cx| {
                        view.toggle_content_search_mode(
                            content_search::SidebarMode::Content,
                            window,
                            cx,
                        );
                    })),
            );
        // A row of its own: at `SIDEBAR_MIN_WIDTH` there is not enough room
        // left in `toolbar` for the sort control to stay fully visible and
        // clickable alongside the other toolbar buttons (CodeRabbit review
        // 5482775725 / inline comment 4241076881 on PR #451).
        let sort_row = div()
            .id("work-folder-sort-row")
            .debug_selector(|| "sidebar-sort-row".to_owned())
            .h(px(SIDEBAR_TOOLBAR_HEIGHT))
            .flex_none()
            .flex()
            .flex_row()
            .items_center()
            .mb(px(SIDEBAR_TOOLBAR_GAP))
            .child(sort_button);
        let mut filter_input = div().relative().flex_1().min_w(px(0.0)).h_full();
        if self.sidebar_filter.text().is_empty() {
            filter_input = filter_input.child(
                div()
                    .absolute()
                    .inset_0()
                    .flex()
                    .items_center()
                    .text_color(rgb(self.theme.quote_foreground))
                    .child("Filter files…"),
            );
        }
        filter_input = filter_input.child(InlineRenameInput { input: cx.entity() });
        let show_filter = !self.inline_rename_active();
        let filter = show_filter.then(|| {
            div()
                .id("work-folder-file-filter")
                .debug_selector(|| "sidebar-filter".to_owned())
                .h(px(SIDEBAR_FILTER_HEIGHT))
                .flex_none()
                .mb(px(SIDEBAR_FILTER_GAP))
                .px(px(6.0))
                .rounded_sm()
                .border_1()
                .border_color(rgb(self.theme.sidebar_foreground))
                .bg(rgb(self.theme.code_background))
                .cursor_pointer()
                .on_mouse_down(MouseButton::Left, cx.listener(Self::focus_sidebar_filter))
                .child(filter_input)
        });
        let root_is_selected = (self.sidebar_focus == SidebarFocus::Folder
            && self.selected_folder.is_none())
            || (self.sidebar_focus == SidebarFocus::ActiveSession
                && !self.active_session_has_sidebar_row());
        let root_row = div()
            .id("work-folder-root")
            .debug_selector(|| "sidebar-root".to_owned())
            .h(px(SIDEBAR_ROW_HEIGHT))
            .px(px(SIDEBAR_ROW_HORIZONTAL_PADDING))
            .rounded_sm()
            .cursor_pointer()
            .when(root_is_selected, |element| {
                element.bg(rgb(self.theme.sidebar_active_background))
            })
            .child(
                div()
                    .h_full()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_1()
                    .child(entry_icon(
                        icons::ICON_FOLDER,
                        None,
                        self.theme.sidebar_foreground,
                    ))
                    .child(work_folder_root_display_name(work_folder.root())),
            )
            .on_click(cx.listener(|view, _, window, cx| {
                window.focus(&view.focus_handle, cx);
                view.select_work_folder_root(cx);
            }));
        let query = self.sidebar_filter.text().to_lowercase();
        let filtering = !query.is_empty();
        let mut rows = Vec::new();
        if filtering {
            flatten_filtered_work_folder_tree(work_folder.children(), 1, &query, &mut rows);
        } else {
            flatten_work_folder_tree(work_folder.children(), 1, &self.expanded_folders, &mut rows);
        }
        let tree_row_count = rows.len();
        let empty_filter_row = filtering && tree_row_count == 0;
        let today = local_today();
        let tree = rows
            .into_iter()
            .enumerate()
            .map(|(index, row)| {
                let left_padding = px(SIDEBAR_ROW_HORIZONTAL_PADDING + 12.0 * row.depth as f32);
                match row.node {
                    WorkFolderNode::File(entry) => {
                        let is_active = self.sidebar_focus == SidebarFocus::ActiveSession
                            && active_path == Some(entry.path());
                        let path = entry.path().to_path_buf();
                        let is_renaming = self
                            .inline_rename
                            .as_ref()
                            .is_some_and(|rename| rename.from == path);
                        let name = if is_renaming {
                            self.inline_rename_label(cx)
                        } else {
                            file_name_label(
                                entry.file_name(),
                                today,
                                &self.theme,
                                DateBadgePosition::Right,
                            )
                        };
                        let menu_path = path.clone();
                        div()
                            .id(("work-folder-entry", index))
                            .on_mouse_down(
                                MouseButton::Right,
                                cx.listener(move |view, event: &MouseDownEvent, _, cx| {
                                    view.open_sidebar_context_menu(
                                        SidebarContextTarget::File(menu_path.clone()),
                                        event.position,
                                        cx,
                                    );
                                }),
                            )
                            .debug_selector(|| "sidebar-file".to_owned())
                            .h(px(SIDEBAR_ROW_HEIGHT))
                            .pl(left_padding)
                            .pr(px(SIDEBAR_ROW_HORIZONTAL_PADDING))
                            .rounded_sm()
                            .cursor_pointer()
                            .when(is_active, |element| {
                                element.bg(rgb(self.theme.sidebar_active_background))
                            })
                            .child(
                                div()
                                    .h_full()
                                    .flex()
                                    .flex_row()
                                    .items_center()
                                    .gap_1()
                                    .child(entry_icon(
                                        icons::ICON_FILE,
                                        None,
                                        self.theme.sidebar_foreground,
                                    ))
                                    .child(name),
                            )
                            .on_click(cx.listener(move |view, event: &ClickEvent, window, cx| {
                                window.focus(&view.focus_handle, cx);
                                if !event.is_keyboard() && event.click_count() >= 2 {
                                    view.sidebar_keyboard_focus = true;
                                    view.begin_inline_rename(
                                        path.clone(),
                                        InlineRenameKind::File,
                                        window,
                                        cx,
                                    );
                                } else if view
                                    .inline_rename
                                    .as_ref()
                                    .is_some_and(|rename| rename.from == path)
                                {
                                    window.focus(&view.focus_handle, cx);
                                    if let Some(position) = event.mouse_position() {
                                        view.move_inline_rename_to_point(position, window, cx);
                                    }
                                } else {
                                    view.open_work_folder_entry(&path, cx);
                                }
                            }))
                    }
                    WorkFolderNode::Folder(folder) => {
                        let is_selected = self.sidebar_focus == SidebarFocus::Folder
                            && self.selected_folder.as_deref() == Some(folder.path());
                        let is_expanded = self.expanded_folders.contains(folder.path());
                        let path = folder.path().to_path_buf();
                        let is_renaming = self
                            .inline_rename
                            .as_ref()
                            .is_some_and(|rename| rename.from == path);
                        let disclosure = if is_expanded {
                            icons::ICON_CHEVRON_DOWN
                        } else {
                            icons::ICON_CHEVRON_RIGHT
                        };
                        let name = if is_renaming {
                            self.inline_rename_label(cx)
                        } else {
                            div()
                                .flex_1()
                                .min_w(px(0.0))
                                .child(folder.name().to_owned())
                        };
                        let menu_path = path.clone();
                        div()
                            .id(("work-folder-folder", index))
                            .on_mouse_down(
                                MouseButton::Right,
                                cx.listener(move |view, event: &MouseDownEvent, _, cx| {
                                    view.open_sidebar_context_menu(
                                        SidebarContextTarget::Folder(menu_path.clone()),
                                        event.position,
                                        cx,
                                    );
                                }),
                            )
                            .debug_selector(|| "sidebar-folder".to_owned())
                            .h(px(SIDEBAR_ROW_HEIGHT))
                            .pl(left_padding)
                            .pr(px(SIDEBAR_ROW_HORIZONTAL_PADDING))
                            .rounded_sm()
                            .cursor_pointer()
                            .when(is_selected, |element| {
                                element.bg(rgb(self.theme.sidebar_active_background))
                            })
                            .child(
                                div()
                                    .h_full()
                                    .flex()
                                    .flex_row()
                                    .items_center()
                                    .gap_1()
                                    .child(entry_icon(
                                        icons::ICON_FOLDER,
                                        Some(disclosure),
                                        self.theme.sidebar_foreground,
                                    ))
                                    .child(name),
                            )
                            .on_click(cx.listener(move |view, event: &ClickEvent, window, cx| {
                                window.focus(&view.focus_handle, cx);
                                if !event.is_keyboard() && event.click_count() >= 2 {
                                    view.sidebar_keyboard_focus = true;
                                    view.begin_inline_rename(
                                        path.clone(),
                                        InlineRenameKind::Folder,
                                        window,
                                        cx,
                                    );
                                } else if view
                                    .inline_rename
                                    .as_ref()
                                    .is_some_and(|rename| rename.from == path)
                                {
                                    window.focus(&view.focus_handle, cx);
                                    if let Some(position) = event.mouse_position() {
                                        view.move_inline_rename_to_point(position, window, cx);
                                    }
                                } else if view.sidebar_filter.text().is_empty() {
                                    view.toggle_and_select_work_folder_folder(path.clone(), cx);
                                } else if view.cancel_inline_rename(cx) {
                                    view.blur_sidebar_filter(cx);
                                    view.sidebar_keyboard_focus = true;
                                    view.selected_folder = Some(path.clone());
                                    view.sidebar_focus = SidebarFocus::Folder;
                                    cx.notify();
                                }
                            }))
                    }
                }
            })
            .collect::<Vec<_>>();
        let empty_filter = empty_filter_row.then(|| {
            div()
                .id("work-folder-filter-empty")
                .debug_selector(|| "sidebar-filter-empty".to_owned())
                .h(px(SIDEBAR_ROW_HEIGHT))
                .px(px(SIDEBAR_ROW_HORIZONTAL_PADDING))
                .flex()
                .items_center()
                .text_color(rgb(self.theme.quote_foreground))
                .child("No matching files")
        });
        let mut draft_ids: Vec<SessionId> = self.work_folder_drafts.keys().copied().collect();
        draft_ids.sort_by_key(|id| id.0);
        let draft_row_count = draft_ids
            .iter()
            .filter(|id| self.sessions.get(**id).is_some())
            .count();
        let drafts = draft_ids
            .into_iter()
            .filter_map(|id| self.sessions.get(id).map(|session| (id, session)))
            .map(|(id, session)| {
                let is_active =
                    self.sidebar_focus == SidebarFocus::ActiveSession && active_id == id;
                div()
                    .id(("work-folder-draft", id.0 as usize))
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |view, event: &MouseDownEvent, _, cx| {
                            if let Some(instance) = view.document_instance(id) {
                                view.open_sidebar_context_menu(
                                    SidebarContextTarget::Draft(id, instance),
                                    event.position,
                                    cx,
                                );
                            }
                        }),
                    )
                    .debug_selector(|| "sidebar-draft".to_owned())
                    .h(px(SIDEBAR_ROW_HEIGHT))
                    .px(px(SIDEBAR_ROW_HORIZONTAL_PADDING))
                    .rounded_sm()
                    .cursor_pointer()
                    .when(is_active, |element| {
                        element.bg(rgb(self.theme.sidebar_active_background))
                    })
                    .child(
                        div()
                            .h_full()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_1()
                            .child(entry_icon(
                                icons::ICON_FILE,
                                None,
                                self.theme.sidebar_foreground,
                            ))
                            .child(draft_preview(session)),
                    )
                    .on_click(cx.listener(move |view, _, _, cx| {
                        if !view.cancel_inline_rename(cx) {
                            return;
                        }
                        view.blur_sidebar_filter(cx);
                        view.sidebar_keyboard_focus = true;
                        view.activate_session(id, cx);
                    }))
            })
            .collect::<Vec<_>>();
        let content_height =
            sidebar_list_content_height(tree_row_count, draft_row_count, empty_filter_row);
        let list_top = SIDEBAR_PADDING
            + SIDEBAR_TOOLBAR_HEIGHT
            + SIDEBAR_TOOLBAR_GAP
            + SIDEBAR_TOOLBAR_HEIGHT
            + SIDEBAR_TOOLBAR_GAP
            + if show_filter {
                SIDEBAR_FILTER_HEIGHT + SIDEBAR_FILTER_GAP
            } else {
                0.0
            };
        let list_viewport_height = (sidebar_viewport_height
            - list_top
            - SIDEBAR_SETTINGS_GAP
            - SIDEBAR_SETTINGS_HEIGHT
            - SIDEBAR_PADDING)
            .max(0.0);
        let scrollbar = self.sidebar_scrollbar(list_top, list_viewport_height, content_height, cx);
        let list = div()
            .id("work-folder-sidebar-list")
            .debug_selector(|| "sidebar-list".to_owned())
            .flex_1()
            .overflow_y_scroll()
            .track_scroll(&self.sidebar_scroll)
            .on_scroll_wheel(cx.listener(|view, _, _, cx| view.show_sidebar_scrollbar_briefly(cx)))
            .child(root_row)
            .children(tree)
            .children(empty_filter)
            .children(drafts);
        let view = cx.entity();
        let settings_button = div()
            .id("sidebar-settings")
            .w_full()
            .h_full()
            .flex()
            .flex_row()
            .items_center()
            .gap_1()
            .px_2()
            .rounded_sm()
            .cursor_pointer()
            .hover(|style| style.bg(rgb(self.theme.sidebar_active_background)))
            .child(
                gpui::svg()
                    .path(icons::ICON_SETTINGS)
                    .size_4()
                    .flex_none()
                    .text_color(rgb(self.theme.sidebar_foreground)),
            )
            .child("設定")
            .on_click(move |_, window, app| {
                view.update(app, |view, cx| view.open_settings(window, cx));
            });
        let settings_footer = div()
            .id("sidebar-settings-footer")
            .debug_selector(|| "sidebar-settings-footer".to_owned())
            .h(px(SIDEBAR_SETTINGS_HEIGHT))
            .mt(px(SIDEBAR_SETTINGS_GAP))
            .flex_none()
            .child(settings_button);
        Some(
            div()
                .id("work-folder-panel")
                .relative()
                .flex_none()
                .w(px(self.sidebar_width))
                .h_full()
                .overflow_hidden()
                .bg(rgb(self.theme.sidebar_background))
                .child(
                    div()
                        .id("work-folder-sidebar")
                        .size_full()
                        .flex()
                        .flex_col()
                        .pt(px(SIDEBAR_PADDING))
                        .pb(px(SIDEBAR_PADDING))
                        .pl(px(SIDEBAR_PADDING))
                        .pr(px(SIDEBAR_PADDING + SCROLLBAR_TRACK_WIDTH))
                        .bg(rgb(self.theme.sidebar_background))
                        .text_color(rgb(self.theme.sidebar_foreground))
                        .text_size(px(BODY_FONT_SIZE))
                        .child(toolbar)
                        .child(sort_row)
                        .children(filter)
                        .child(list)
                        .child(settings_footer),
                )
                .children(scrollbar),
        )
    }

    pub(super) fn active_session_has_sidebar_row(&self) -> bool {
        self.work_folder_drafts
            .contains_key(&self.sessions.active_id())
            || self.active_session().path().is_some_and(|path| {
                self.work_folder.as_ref().is_some_and(|folder| {
                    let mut rows = Vec::new();
                    flatten_work_folder_tree(
                        folder.children(),
                        1,
                        &self.expanded_folders,
                        &mut rows,
                    );
                    rows.iter().any(|row| {
                        matches!(&row.node, WorkFolderNode::File(entry) if entry.path() == path)
                    })
                })
            })
    }
}

impl EditorView {
    fn open_sidebar_context_menu(
        &mut self,
        target: SidebarContextTarget,
        position: gpui::Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        self.file_tab_context_menu = None;
        self.sidebar_context_menu = Some(SidebarContextMenu { position, target });
        cx.notify();
    }

    pub(crate) fn dismiss_sidebar_context_menu(&mut self, cx: &mut Context<Self>) -> bool {
        if self.sidebar_context_menu.take().is_some() {
            cx.notify();
            true
        } else {
            false
        }
    }

    fn close_sidebar_context_menu(
        &mut self,
        _: &MouseDownEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.dismiss_sidebar_context_menu(cx);
    }

    fn sidebar_context_rename(
        &mut self,
        path: PathBuf,
        kind: InlineRenameKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.sidebar_context_menu = None;
        self.sidebar_keyboard_focus = true;
        self.begin_inline_rename(path, kind, window, cx);
        cx.notify();
    }

    #[cfg(any(target_os = "macos", target_os = "windows"))]
    fn sidebar_context_reveal(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.sidebar_context_menu = None;
        self.reveal_file_tab_in_file_manager(Some(path), cx);
    }

    fn sidebar_context_copy_path(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.sidebar_context_menu = None;
        self.copy_file_tab_full_path(Some(path), cx);
    }

    /// The session a sidebar Save As acts on, resolved from the clicked row.
    /// A file row that is not open has no document to save, so it yields
    /// `None` rather than loading one on the pointer path.
    fn sidebar_save_as_session(&self, target: &SidebarContextTarget) -> Option<SessionId> {
        match target {
            SidebarContextTarget::File(path) => self.sessions.session_for_path(path),
            SidebarContextTarget::Draft(id, instance) => {
                (self.document_instance(*id) == Some(*instance)).then_some(*id)
            }
            SidebarContextTarget::Folder(_) => None,
        }
    }

    fn sidebar_context_save_as(&mut self, target: SidebarContextTarget, cx: &mut Context<Self>) {
        self.sidebar_context_menu = None;
        match self.sidebar_save_as_session(&target) {
            Some(id) => self.prompt_save_as_for(id, cx),
            None => {
                self.status = Some("Save As するにはファイルを開いてください".to_owned());
            }
        }
        cx.notify();
    }

    /// Moves a file to the OS Trash / Recycle Bin on a background thread. A
    /// session with unsaved changes (or a write in flight) blocks the move
    /// instead of being discarded; if edits land while the move is running the
    /// session stays open with its content, marked as missing from disk.
    fn sidebar_context_trash(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.sidebar_context_menu = None;
        if !self.cancel_inline_rename(cx) {
            return;
        }
        if let Some(session) = self
            .sessions
            .session_for_path(&path)
            .and_then(|id| self.sessions.get(id))
            && (session.is_dirty() || session.save_in_flight())
        {
            self.status =
                Some("未保存の変更があるため、ゴミ箱へ移動できません。先に保存してください".to_owned());
            cx.notify();
            return;
        }
        self.status = Some("ゴミ箱へ移動しています…".to_owned());
        let generation = self.work_folder_generation;
        cx.spawn(async move |view, cx| {
            let trash_path = path.clone();
            let result = cx
                .background_executor()
                .spawn(async move { move_path_to_trash(&trash_path) })
                .await;
            let _ = view.update(cx, |view, cx| {
                if view.work_folder_generation != generation {
                    return;
                }
                match result {
                    Ok(()) => {
                        view.sessions
                            .apply_file_event(&FileEvent::Deleted(path.clone()));
                        if let Some(folder) = view.work_folder.as_mut() {
                            folder.remove(&path);
                        }
                        view.content_search_workspace_changed(cx);
                        view.status = Some("ゴミ箱へ移動しました".to_owned());
                    }
                    Err(error) => {
                        view.status = Some(format!("ゴミ箱へ移動できません: {error}"));
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// Moves a whole folder to the OS Trash / Recycle Bin on a background
    /// thread. Any session under it with unsaved changes (or a write in
    /// flight) blocks the move; if edits land while the move is running the
    /// sessions stay open with their content, marked as missing from disk.
    /// The Work Folder root is never a target.
    fn sidebar_context_trash_folder(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.sidebar_context_menu = None;
        if !self.cancel_inline_rename(cx) {
            return;
        }
        let Some(root) = self.work_folder.as_ref().map(|folder| folder.root().to_path_buf())
        else {
            return;
        };
        if path == root || !path.starts_with(&root) {
            self.status = Some("このフォルダはゴミ箱へ移動できません".to_owned());
            cx.notify();
            return;
        }
        if self.sessions.has_unsaved_under(&path) {
            self.status = Some(
                "未保存の変更があるファイルを含むため、ゴミ箱へ移動できません。先に保存してください"
                    .to_owned(),
            );
            cx.notify();
            return;
        }
        self.status = Some("ゴミ箱へ移動しています…".to_owned());
        let generation = self.work_folder_generation;
        cx.spawn(async move |view, cx| {
            let trash_path = path.clone();
            let result = cx
                .background_executor()
                .spawn(async move { move_path_to_trash(&trash_path) })
                .await;
            let _ = view.update(cx, |view, cx| {
                if view.work_folder_generation != generation {
                    return;
                }
                match result {
                    Ok(()) => {
                        view.sessions.delete_folder(&path);
                        if let Some(folder) = view.work_folder.as_mut() {
                            folder.remove_folder(&path);
                        }
                        view.expanded_folders
                            .retain(|expanded| !expanded.starts_with(&path));
                        view.pending_new_folders
                            .retain(|pending| !pending.starts_with(&path));
                        if view
                            .selected_folder
                            .as_ref()
                            .is_some_and(|selected| selected.starts_with(&path))
                        {
                            view.selected_folder = None;
                            if view.sidebar_focus == SidebarFocus::Folder {
                                view.sidebar_focus = SidebarFocus::ActiveSession;
                            }
                        }
                        view.content_search_workspace_changed(cx);
                        view.status = Some("ゴミ箱へ移動しました".to_owned());
                    }
                    Err(error) => {
                        view.status = Some(format!("ゴミ箱へ移動できません: {error}"));
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn sidebar_context_menu_element(
        &self,
        menu: &SidebarContextMenu,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let mut items: Vec<gpui::Stateful<gpui::Div>> = Vec::new();
        match &menu.target {
            SidebarContextTarget::File(path) => {
                let rename_path = path.clone();
                items.push(self.file_tab_context_menu_item(
                    "sidebar-context-rename",
                    "名前を変更",
                    true,
                    cx,
                    move |view, _, window, cx| {
                        view.sidebar_context_rename(
                            rename_path.clone(),
                            InlineRenameKind::File,
                            window,
                            cx,
                        );
                    },
                ));
                let trash_path = path.clone();
                items.push(self.file_tab_context_menu_item(
                    "sidebar-context-trash",
                    "ゴミ箱に移動",
                    true,
                    cx,
                    move |view, _, _, cx| view.sidebar_context_trash(trash_path.clone(), cx),
                ));
                #[cfg(any(target_os = "macos", target_os = "windows"))]
                {
                    let reveal_path = path.clone();
                    items.push(self.file_tab_context_menu_item(
                        "sidebar-context-reveal",
                        REVEAL_IN_FILE_MANAGER_LABEL,
                        true,
                        cx,
                        move |view, _, _, cx| view.sidebar_context_reveal(reveal_path.clone(), cx),
                    ));
                }
                let copy_path = path.clone();
                items.push(self.file_tab_context_menu_item(
                    "sidebar-context-copy-path",
                    "フルパスをコピー",
                    true,
                    cx,
                    move |view, _, _, cx| view.sidebar_context_copy_path(copy_path.clone(), cx),
                ));
                let save_as_target = menu.target.clone();
                items.push(self.file_tab_context_menu_item(
                    "sidebar-context-save-as",
                    "名前を付けて保存…",
                    self.sidebar_save_as_session(&menu.target).is_some(),
                    cx,
                    move |view, _, _, cx| {
                        view.sidebar_context_save_as(save_as_target.clone(), cx)
                    },
                ));
            }
            SidebarContextTarget::Folder(path) => {
                let rename_path = path.clone();
                items.push(self.file_tab_context_menu_item(
                    "sidebar-context-rename",
                    "名前を変更",
                    true,
                    cx,
                    move |view, _, window, cx| {
                        view.sidebar_context_rename(
                            rename_path.clone(),
                            InlineRenameKind::Folder,
                            window,
                            cx,
                        );
                    },
                ));
                let trash_path = path.clone();
                items.push(self.file_tab_context_menu_item(
                    "sidebar-context-trash",
                    "ゴミ箱に移動",
                    true,
                    cx,
                    move |view, _, _, cx| {
                        view.sidebar_context_trash_folder(trash_path.clone(), cx)
                    },
                ));
                #[cfg(any(target_os = "macos", target_os = "windows"))]
                {
                    let reveal_path = path.clone();
                    items.push(self.file_tab_context_menu_item(
                        "sidebar-context-reveal",
                        REVEAL_IN_FILE_MANAGER_LABEL,
                        true,
                        cx,
                        move |view, _, _, cx| view.sidebar_context_reveal(reveal_path.clone(), cx),
                    ));
                }
                let copy_path = path.clone();
                items.push(self.file_tab_context_menu_item(
                    "sidebar-context-copy-path",
                    "フルパスをコピー",
                    true,
                    cx,
                    move |view, _, _, cx| view.sidebar_context_copy_path(copy_path.clone(), cx),
                ));
            }
            SidebarContextTarget::Draft(..) => {
                let save_as_target = menu.target.clone();
                items.push(self.file_tab_context_menu_item(
                    "sidebar-context-save-as",
                    "名前を付けて保存…",
                    self.sidebar_save_as_session(&menu.target).is_some(),
                    cx,
                    move |view, _, _, cx| {
                        view.sidebar_context_save_as(save_as_target.clone(), cx)
                    },
                ));
            }
        }
        div()
            .id("sidebar-context-menu")
            .debug_selector(|| "sidebar-context-menu".to_owned())
            .occlude()
            .min_w(px(180.0))
            .flex()
            .flex_col()
            .p_1()
            .rounded_sm()
            .border_1()
            .border_color(rgb(self.theme.header_foreground))
            .bg(rgb(self.theme.code_background))
            .text_color(rgb(self.theme.foreground))
            .on_mouse_down_out(cx.listener(Self::close_sidebar_context_menu))
            .children(items)
    }
}

/// Asks the OS to move `path` to its Trash / Recycle Bin. Never falls back to
/// a permanent delete: a failure is reported and the file is left in place.
///
/// Success is returned only when the OS operation reports success and the
/// original path is confirmed gone; the caller emits `FileEvent::Deleted` on
/// `Ok` only.
fn move_path_to_trash(path: &Path) -> std::io::Result<()> {
    move_path_to_trash_with(path, native_move_to_trash)
}

/// Confirms the source exists, runs `operation`, then confirms the source is
/// gone. Only a `NotFound` after the operation counts as a confirmed move; a
/// missing source beforehand, a failed operation, a remaining source, or any
/// other I/O error while checking is reported as a failure.
fn move_path_to_trash_with(
    path: &Path,
    operation: impl FnOnce(&Path) -> std::io::Result<()>,
) -> std::io::Result<()> {
    path.symlink_metadata()?;
    operation(path)?;
    match path.symlink_metadata() {
        Ok(_) => Err(std::io::Error::other(
            "ゴミ箱への移動を確認できませんでした（元のファイルが残っています）",
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

/// macOS: `NSFileManager trashItemAtURL:resultingItemURL:error:`, without
/// Finder / Apple Events. `NO` or a missing resulting URL is an error.
#[cfg(target_os = "macos")]
fn native_move_to_trash(path: &Path) -> std::io::Result<()> {
    use objc::runtime::{BOOL, NO};
    use std::ffi::{CStr, c_char, c_void};
    use std::os::unix::ffi::OsStrExt;

    type Id = *mut c_void;

    #[link(name = "Foundation", kind = "framework")]
    unsafe extern "C" {}

    #[link(name = "objc")]
    unsafe extern "C" {
        fn objc_getClass(name: *const c_char) -> Id;
        fn sel_registerName(name: *const c_char) -> Id;
        // Variadic in C; each call site casts it to the exact signature.
        fn objc_msgSend();
    }

    /// Sends `selector` to `receiver` through `objc_msgSend` cast to the
    /// exact C signature of the method (`$ret` result, `$ty` arguments).
    macro_rules! send {
        ($ret:ty; $receiver:expr, $selector:expr $(, $ty:ty = $arg:expr)*) => {{
            let call = std::mem::transmute::<
                unsafe extern "C" fn(),
                unsafe extern "C" fn(Id, Id $(, $ty)*) -> $ret,
            >(objc_msgSend as unsafe extern "C" fn());
            call($receiver, sel_registerName($selector.as_ptr()) $(, $arg)*)
        }};
    }

    const NS_UTF8_STRING_ENCODING: usize = 4;
    let bytes = path.as_os_str().as_bytes();
    // SAFETY: plain Objective-C message sends to Foundation classes with
    // correctly typed arguments; every object created here is released (or
    // owned by the pool drained) before returning.
    unsafe {
        let pool = send!(Id; objc_getClass(c"NSAutoreleasePool".as_ptr()), c"new");
        let result = (|| {
            let string = send!(Id; objc_getClass(c"NSString".as_ptr()), c"alloc");
            let string = send!(
                Id; string, c"initWithBytes:length:encoding:",
                *const u8 = bytes.as_ptr(),
                usize = bytes.len(),
                usize = NS_UTF8_STRING_ENCODING
            );
            if string.is_null() {
                return Err(std::io::Error::other("パスをNSStringに変換できません"));
            }
            let url = send!(
                Id; objc_getClass(c"NSURL".as_ptr()), c"fileURLWithPath:",
                Id = string
            );
            send!((); string, c"release");
            if url.is_null() {
                return Err(std::io::Error::other("パスをNSURLに変換できません"));
            }
            let manager = send!(Id; objc_getClass(c"NSFileManager".as_ptr()), c"defaultManager");
            let mut resulting: Id = std::ptr::null_mut();
            let mut error: Id = std::ptr::null_mut();
            let moved = send!(
                BOOL; manager, c"trashItemAtURL:resultingItemURL:error:",
                Id = url,
                *mut Id = &mut resulting,
                *mut Id = &mut error
            );
            if moved == NO || resulting.is_null() {
                let message = if error.is_null() {
                    "ゴミ箱への移動に失敗しました".to_owned()
                } else {
                    let description = send!(Id; error, c"localizedDescription");
                    let utf8: *const c_char = if description.is_null() {
                        std::ptr::null()
                    } else {
                        send!(*const c_char; description, c"UTF8String")
                    };
                    if utf8.is_null() {
                        "ゴミ箱への移動に失敗しました".to_owned()
                    } else {
                        CStr::from_ptr(utf8).to_string_lossy().into_owned()
                    }
                };
                return Err(std::io::Error::other(message));
            }
            Ok(())
        })();
        send!((); pool, c"drain");
        result
    }
}

/// Windows: `IFileOperation` with `FOFX_RECYCLEONDELETE`. API errors and an
/// aborted operation are errors; there is no permanent-delete fallback.
#[cfg(target_os = "windows")]
fn native_move_to_trash(path: &Path) -> std::io::Result<()> {
    use windows::Win32::Foundation::RPC_E_CHANGED_MODE;
    use windows::Win32::System::Com::{
        CLSCTX_ALL, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoCreateInstance,
        CoInitializeEx, CoUninitialize,
    };
    use windows::Win32::UI::Shell::{
        FOF_ALLOWUNDO, FOF_NOERRORUI, FOF_SILENT, FOFX_EARLYFAILURE, FOFX_RECYCLEONDELETE,
        FileOperation, IFileOperation, IShellItem, SHCreateItemFromParsingName,
    };
    use windows::core::HSTRING;

    fn to_io(error: windows::core::Error) -> std::io::Error {
        std::io::Error::other(error.to_string())
    }

    fn recycle(path: &Path) -> std::io::Result<()> {
        // SAFETY: COM calls on an initialized apartment; all interface
        // pointers are owned by the `windows` crate wrappers.
        unsafe {
            let operation: IFileOperation =
                CoCreateInstance(&FileOperation, None, CLSCTX_ALL).map_err(to_io)?;
            operation
                .SetOperationFlags(
                    FOF_ALLOWUNDO
                        | FOF_NOERRORUI
                        | FOF_SILENT
                        | FOFX_EARLYFAILURE
                        | FOFX_RECYCLEONDELETE,
                )
                .map_err(to_io)?;
            let item: IShellItem =
                SHCreateItemFromParsingName(&HSTRING::from(path.as_os_str()), None)
                    .map_err(to_io)?;
            operation.DeleteItem(&item, None).map_err(to_io)?;
            operation.PerformOperations().map_err(to_io)?;
            if operation.GetAnyOperationsAborted().map_err(to_io)?.as_bool() {
                return Err(std::io::Error::other(
                    "ごみ箱への移動が中断されました",
                ));
            }
            Ok(())
        }
    }

    // SAFETY: balanced with `CoUninitialize` below when initialization
    // succeeded; a mode conflict means COM is already usable on this thread.
    let init = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) };
    let initialized = if init.is_ok() {
        true
    } else if init == RPC_E_CHANGED_MODE {
        false
    } else {
        return Err(std::io::Error::other(init.message()));
    };
    let result = recycle(path);
    if initialized {
        // SAFETY: matches the successful `CoInitializeEx` above.
        unsafe { CoUninitialize() };
    }
    result
}

/// Linux and others: `gio trash`.
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn native_move_to_trash(path: &Path) -> std::io::Result<()> {
    let output = Command::new("gio")
        .arg("trash")
        .arg("--")
        .arg(path)
        .output()?;
    if output.status.success() {
        Ok(())
    } else {
        Err(std::io::Error::other(
            String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        ))
    }
}

/// The sidebar label for the work folder root: its own directory name, or
/// the full path if it has none (a filesystem root such as `/`).
fn work_folder_root_display_name(root: &Path) -> String {
    root.file_name().map_or_else(
        || root.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

/// Where the date badge renders relative to the rest of a file-row label's
/// text. Both sides render through the same [`file_name_label`] call; only
/// `Right` is wired to the one current call site today, with `Left` kept
/// selectable for a future settings screen that lets the user choose.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DateBadgePosition {
    /// Reserved for a future settings toggle; exercised today by
    /// [`badge_renders_before_remainder`]'s unit tests.
    #[allow(dead_code)]
    Left,
    Right,
}

/// Whether the date badge should render before the remainder text for a
/// given [`DateBadgePosition`]: pure so both orderings are unit-testable
/// without a GPUI context.
fn badge_renders_before_remainder(position: DateBadgePosition) -> bool {
    matches!(position, DateBadgePosition::Left)
}

/// The file name as shown in a sidebar row: a trailing `.md` extension
/// (ASCII case-insensitive, matching what the work folder accepts) is
/// omitted. Display only — the real file name and path are untouched. A name
/// that is nothing but the extension is kept as is so the row is never blank.
fn display_file_name(file_name: &str) -> &str {
    let Some(stem_len) = file_name.len().checked_sub(3) else {
        return file_name;
    };
    if stem_len == 0 || !file_name.is_char_boundary(stem_len) {
        return file_name;
    }
    if file_name[stem_len..].eq_ignore_ascii_case(".md") {
        &file_name[..stem_len]
    } else {
        file_name
    }
}

/// The sidebar's file-row label: the file name (minus a trailing `.md`) unchanged, unless it embeds
/// a recognized date (`YYYY-MM-DD` or `YYYYMMDD`), in which case that date
/// renders as its own small badge (`本日`, `17日(木)`, `10/3(土)`,
/// `2025/10/3(金)`, …) instead of raw digits, positioned per
/// `badge_position` relative to the rest of the name joined back into one
/// natural, readable string — while the surrounding text stays exactly what
/// the file system reports — the real file name, path, and work-folder sort
/// key never change.
fn file_name_label(
    file_name: &str,
    today: CalendarDate,
    theme: &Theme,
    badge_position: DateBadgePosition,
) -> gpui::Div {
    // Lets this label shrink below its content width inside the sidebar's
    // fixed-width row instead of pushing the (flex_none) date badge past the
    // visible edge; `min_w` defaults to the content size otherwise.
    let row = div()
        .flex()
        .flex_row()
        .items_center()
        .gap_1()
        .min_w(px(0.0));
    let file_name = display_file_name(file_name);
    let Some(badge) = split_file_name_for_badge(file_name) else {
        return row.child(file_name.to_owned());
    };
    let remainder = badge.remainder();
    let chip = date_badge_chip(badge.date, today, theme);
    let remainder_child =
        (!remainder.is_empty()).then(|| div().flex_1().min_w(px(0.0)).truncate().child(remainder));
    if badge_renders_before_remainder(badge_position) {
        row.child(chip).children(remainder_child)
    } else {
        row.children(remainder_child).child(chip)
    }
}

const DATE_BADGE_TODAY_BACKGROUND: u32 = 0x44779e;
const DATE_BADGE_THIS_WEEK_BACKGROUND: u32 = 0x356d94;
const DATE_BADGE_THIS_MONTH_BACKGROUND: u32 = 0x2e6287;
const DATE_BADGE_THIS_YEAR_BACKGROUND: u32 = 0x285878;
const DATE_BADGE_OTHER_BACKGROUND: u32 = 0x214c68;
const DATE_BADGE_FOREGROUND: u32 = 0xf7fbff;

fn date_badge_background(date: CalendarDate, today: CalendarDate) -> u32 {
    match date_badge_range(date, today) {
        DateBadgeRange::Today => DATE_BADGE_TODAY_BACKGROUND,
        DateBadgeRange::ThisWeek => DATE_BADGE_THIS_WEEK_BACKGROUND,
        DateBadgeRange::ThisMonth => DATE_BADGE_THIS_MONTH_BACKGROUND,
        DateBadgeRange::ThisYear => DATE_BADGE_THIS_YEAR_BACKGROUND,
        DateBadgeRange::Other => DATE_BADGE_OTHER_BACKGROUND,
    }
}

/// A small rounded chip for one badge-worthy date. Its background follows
/// Hane's feather blues: today is the brightest tier, then the same week,
/// month, year, and finally all other dates become progressively darker. The
/// luminance steps are intentionally wider than the original palette so the
/// order remains legible on a small sidebar chip while the light foreground
/// still has sufficient contrast on every tier.
fn date_badge_chip(date: CalendarDate, today: CalendarDate, _theme: &Theme) -> gpui::Div {
    div()
        .flex_none()
        .px(px(4.0))
        .rounded_sm()
        .bg(rgb(date_badge_background(date, today)))
        .text_color(rgb(DATE_BADGE_FOREGROUND))
        .text_size(px(10.0))
        .child(format_relative_date_label(date, today))
}

/// A short label for an unnamed note in the sidebar: its first non-blank
/// line (an H1 marker stripped, since that will become its title), or a
/// placeholder for a note that is still completely empty.
fn draft_preview(session: &DocumentSession) -> String {
    let text = session.editor().document().full_text();
    let first_line = text.lines().map(str::trim).find(|line| !line.is_empty());
    match first_line {
        Some(line) => line.trim_start_matches('#').trim().to_owned(),
        None => "Untitled note".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_file_name_omits_only_a_trailing_md_extension() {
        assert_eq!(display_file_name("note.md"), "note");
        assert_eq!(display_file_name("NOTE.MD"), "NOTE");
        assert_eq!(display_file_name("Note.Md"), "Note");
        assert_eq!(display_file_name("日本語のメモ.md"), "日本語のメモ");
        assert_eq!(display_file_name("a.md.md"), "a.md");
        assert_eq!(display_file_name("a.md.txt"), "a.md.txt");
        assert_eq!(display_file_name("a.mdx"), "a.mdx");
        assert_eq!(display_file_name("my.md notes"), "my.md notes");
        assert_eq!(display_file_name("md"), "md");
        assert_eq!(display_file_name("日本"), "日本");
        assert_eq!(display_file_name(".md"), ".md");
        assert_eq!(display_file_name(""), "");
    }

    #[test]
    fn display_file_name_keeps_date_badge_split_and_title() {
        let badge = split_file_name_for_badge(display_file_name("2025-10-03_会議メモ.md")).unwrap();
        assert_eq!(badge.remainder(), "会議メモ");
        let badge = split_file_name_for_badge(display_file_name("会議_20251003.MD")).unwrap();
        assert_eq!(badge.remainder(), "会議");
        let badge = split_file_name_for_badge(display_file_name("2025-10-03.md")).unwrap();
        assert_eq!(badge.remainder(), "");
        assert_eq!(badge.date.day, 3);
    }

    fn trash_test_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("hane-trash-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn trash_succeeds_when_operation_removes_existing_path() {
        let dir = trash_test_dir("removed");
        let path = dir.join("note.md");
        std::fs::write(&path, "x").unwrap();
        let result = move_path_to_trash_with(&path, |path| std::fs::remove_file(path));
        assert!(result.is_ok());
        assert!(!path.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn trash_reports_os_operation_error() {
        let dir = trash_test_dir("op-error");
        let path = dir.join("note.md");
        std::fs::write(&path, "x").unwrap();
        let result = move_path_to_trash_with(&path, |_| Err(std::io::Error::other("denied")));
        assert!(result.is_err());
        assert!(path.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn trash_rejects_ok_when_original_path_remains() {
        let dir = trash_test_dir("remains");
        let path = dir.join("note.md");
        std::fs::write(&path, "x").unwrap();
        let result = move_path_to_trash_with(&path, |_| Ok(()));
        assert!(result.is_err());
        assert!(path.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn trash_of_initially_missing_path_is_an_error_without_running_operation() {
        let dir = trash_test_dir("missing");
        let path = dir.join("never-created.md");
        let mut ran = false;
        let result = move_path_to_trash_with(&path, |_| {
            ran = true;
            Ok(())
        });
        assert!(result.is_err());
        assert!(!ran);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn trash_reports_io_error_while_confirming_removal() {
        let dir = trash_test_dir("confirm-error");
        let parent = dir.join("parent");
        std::fs::create_dir(&parent).unwrap();
        let path = parent.join("note.md");
        std::fs::write(&path, "x").unwrap();
        // Replacing the parent directory with a file makes the post-check fail
        // with NotADirectory rather than NotFound.
        let result = move_path_to_trash_with(&path, |_| {
            std::fs::remove_dir_all(&parent)?;
            std::fs::write(&parent, "file")
        });
        let error = result.unwrap_err();
        assert_ne!(error.kind(), std::io::ErrorKind::NotFound);
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn relative_luminance(color: u32) -> f32 {
        fn linear_channel(channel: u32) -> f32 {
            let srgb = ((channel & 0xff) as f32) / 255.0;
            if srgb <= 0.04045 {
                srgb / 12.92
            } else {
                ((srgb + 0.055) / 1.055).powf(2.4)
            }
        }

        0.2126 * linear_channel(color >> 16)
            + 0.7152 * linear_channel(color >> 8)
            + 0.0722 * linear_channel(color)
    }

    #[test]
    fn date_badge_palette_has_visible_steps_and_readable_light_text() {
        let backgrounds = [
            DATE_BADGE_TODAY_BACKGROUND,
            DATE_BADGE_THIS_WEEK_BACKGROUND,
            DATE_BADGE_THIS_MONTH_BACKGROUND,
            DATE_BADGE_THIS_YEAR_BACKGROUND,
            DATE_BADGE_OTHER_BACKGROUND,
        ];
        let luminances: Vec<_> = backgrounds
            .iter()
            .map(|&background| relative_luminance(background))
            .collect();

        for pair in luminances.windows(2) {
            assert!(
                pair[0] > pair[1],
                "date badge palette must darken in proximity order: {pair:?}"
            );
            assert!(
                pair[0] - pair[1] >= 0.02,
                "adjacent date badge colors are too close: {pair:?}"
            );
        }

        let foreground_luminance = relative_luminance(DATE_BADGE_FOREGROUND);
        for (&background, &background_luminance) in backgrounds.iter().zip(&luminances) {
            let contrast = (foreground_luminance + 0.05) / (background_luminance + 0.05);
            assert!(
                contrast >= 4.5,
                "date badge foreground contrast is too low for {background:#08x}: {contrast:.2}"
            );
        }
    }

    #[test]
    fn the_date_badge_renders_before_the_remainder_only_on_the_left() {
        assert!(badge_renders_before_remainder(DateBadgePosition::Left));
        assert!(!badge_renders_before_remainder(DateBadgePosition::Right));
    }
}
