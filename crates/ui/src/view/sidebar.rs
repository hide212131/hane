use super::*;
use gpui_component::Icon;
use hane_session::PinKind;

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

    /// The hover-only "new folder" / "new file" buttons at the right end of a
    /// parent folder row (Issue #642). The slot always takes its space, so the
    /// row and its name never move; the buttons show on row hover (the
    /// `sidebar-pin-row` group). Each button creates inside `directory`, the
    /// row's own folder, whatever is selected. Mouse-down is stopped here so
    /// the row's expand / rename never sees the click.
    fn folder_create_slot(
        &self,
        id: (&'static str, usize),
        directory: PathBuf,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let create_button = |button_id: (&'static str, usize),
                             label: &'static str,
                             icon_path: &'static str| {
            Button::new(button_id)
                .ghost()
                .w(px(sidebar_pins::PIN_SLOT_SIZE))
                .h_full()
                .tooltip(label)
                .accessibility_label(label)
                .icon(
                    Icon::empty()
                        .path(icon_path)
                        .size(px(14.0))
                        .text_color(rgb(self.theme.sidebar_foreground)),
                )
        };
        let folder_view = cx.entity();
        let folder_directory = directory.clone();
        let note_view = cx.entity();
        let slot_id = id;
        div()
            // Keyed per row so tests can tell which row's slot they measure.
            .debug_selector(move || {
                format!("sidebar-folder-create-slot:{}:{}", slot_id.0, slot_id.1)
            })
            .w(px(sidebar_pins::PIN_SLOT_SIZE * 2.0))
            .h(px(sidebar_pins::PIN_SLOT_SIZE))
            .flex_none()
            .flex()
            .flex_row()
            .invisible()
            .group_hover("sidebar-pin-row", |style| style.visible())
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                div()
                    .debug_selector(|| "sidebar-folder-create-folder".to_owned())
                    .child(
                        create_button((id.0, id.1 * 2), "新規フォルダ", icons::ICON_FOLDER_NEW)
                            .on_click(move |_, _, app| {
                                let directory = folder_directory.clone();
                                folder_view.update(app, |view, cx| {
                                    view.new_work_folder_folder_in(Some(directory), cx)
                                });
                            }),
                    ),
            )
            .child(
                div()
                    .debug_selector(|| "sidebar-folder-create-note".to_owned())
                    .child(
                        create_button((id.0, id.1 * 2 + 1), "新規ファイル", icons::ICON_FILE_NEW)
                            .on_click(move |_, _, app| {
                                let directory = directory.clone();
                                note_view.update(app, |view, cx| {
                                    view.new_work_folder_note_in(Some(directory), cx)
                                });
                            }),
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
        let root_create_slot = self.folder_create_slot(
            ("work-folder-root-create", 0),
            work_folder.root().to_path_buf(),
            cx,
        );
        let root_row = div()
            .id("work-folder-root")
            .group("sidebar-pin-row")
            .debug_selector(|| "sidebar-root".to_owned())
            .h(px(SIDEBAR_ROW_HEIGHT))
            .px(px(SIDEBAR_ROW_HORIZONTAL_PADDING))
            .flex()
            .flex_row()
            .items_center()
            .rounded_sm()
            .cursor_pointer()
            .when(root_is_selected, |element| {
                element.bg(rgb(self.theme.sidebar_active_background))
            })
            .child(
                div()
                    .h_full()
                    .flex_1()
                    .min_w(px(0.0))
                    .overflow_hidden()
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
            .child(root_create_slot)
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
        let pinned_relative: HashSet<&Path> = self
            .sidebar_pins
            .entries
            .iter()
            .map(|entry| entry.relative_path.as_path())
            .collect();
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
                        let pin_relative = self.pin_relative(&path);
                        let pinned = pin_relative
                            .as_deref()
                            .is_some_and(|relative| pinned_relative.contains(relative));
                        let pin_slot = self.pin_toggle_slot(
                            ("work-folder-pin-toggle", index),
                            pin_relative,
                            PinKind::File,
                            pinned,
                            cx,
                        );
                        div()
                            .id(("work-folder-entry", index))
                            .group("sidebar-pin-row")
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
                            .flex()
                            .flex_row()
                            .items_center()
                            .rounded_sm()
                            .cursor_pointer()
                            .when(is_active, |element| {
                                element.bg(rgb(self.theme.sidebar_active_background))
                            })
                            .child(
                                div()
                                    .h_full()
                                    .flex_1()
                                    .min_w(px(0.0))
                                    .overflow_hidden()
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
                            .child(pin_slot)
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
                                .truncate()
                                .child(folder.name().to_owned())
                        };
                        let menu_path = path.clone();
                        let pin_relative = self.pin_relative(&path);
                        let pinned = pin_relative
                            .as_deref()
                            .is_some_and(|relative| pinned_relative.contains(relative));
                        let pin_slot = self.pin_toggle_slot(
                            ("work-folder-pin-toggle", index),
                            pin_relative,
                            PinKind::Folder,
                            pinned,
                            cx,
                        );
                        let create_slot = self.folder_create_slot(
                            ("work-folder-create", index),
                            path.clone(),
                            cx,
                        );
                        div()
                            .id(("work-folder-folder", index))
                            .group("sidebar-pin-row")
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
                            .px(px(SIDEBAR_ROW_HORIZONTAL_PADDING))
                            .flex()
                            .flex_row()
                            .items_center()
                            .rounded_sm()
                            .cursor_pointer()
                            .when(is_selected, |element| {
                                element.bg(rgb(self.theme.sidebar_active_background))
                            })
                            .child(
                                div()
                                    .h_full()
                                    .flex_1()
                                    .min_w(px(0.0))
                                    .overflow_hidden()
                                    .flex()
                                    .flex_row()
                                    // The padding must not sit on the
                                    // shrinkable outer area: padding is a
                                    // minimum width, so a deep indent would
                                    // keep it wide and push the create/pin
                                    // slots off the right edge. The outer area
                                    // only clips; the indent lives on the
                                    // inner content.
                                    .child(
                                        div()
                                            .h_full()
                                            .flex_1()
                                            .min_w(px(0.0))
                                            .pl(px(12.0 * row.depth as f32))
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
                                    ),
                            )
                            .child(create_slot)
                            .child(pin_slot)
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
        let controls_height = SIDEBAR_PADDING
            + SIDEBAR_TOOLBAR_HEIGHT
            + SIDEBAR_TOOLBAR_GAP
            + SIDEBAR_TOOLBAR_HEIGHT
            + SIDEBAR_TOOLBAR_GAP
            + if show_filter {
                SIDEBAR_FILTER_HEIGHT + SIDEBAR_FILTER_GAP
            } else {
                0.0
            };
        let footer_height = SIDEBAR_SETTINGS_GAP + SIDEBAR_SETTINGS_HEIGHT + SIDEBAR_PADDING;
        // The pin area sits between the filter and the ordinary list and
        // scrolls on its own; the list's top and viewport (and so its thumb
        // and hit test) start below it. In a region too low for a separate
        // area the pins are the first rows of the list instead.
        let pin_layout = sidebar_pins::pin_area_layout(
            self.sidebar_pins.entries.len(),
            (sidebar_viewport_height - controls_height - footer_height).max(0.0),
        );
        self.sidebar_pins.inline_rows.set(pin_layout.inline_rows);
        let content_height = sidebar_list_content_height(
            tree_row_count + pin_layout.inline_rows,
            draft_row_count,
            empty_filter_row,
        );
        let list_top = controls_height
            + if pin_layout.height > 0.0 {
                pin_layout.height + SIDEBAR_FILTER_GAP
            } else {
                0.0
            };
        let list_viewport_height = (sidebar_viewport_height - list_top - footer_height).max(0.0);
        let mut pin_rows = self.pin_area_rows(cx);
        let inline_pin_rows = if pin_layout.height > 0.0 {
            Vec::new()
        } else {
            std::mem::take(&mut pin_rows)
        };
        let pin_area = (pin_layout.height > 0.0).then(|| {
            div()
                .id("sidebar-pin-area")
                .debug_selector(|| "sidebar-pin-area".to_owned())
                .h(px(pin_layout.height))
                .flex_none()
                .mb(px(SIDEBAR_FILTER_GAP))
                .overflow_y_scroll()
                .track_scroll(&self.sidebar_pins.scroll)
                .children(pin_rows)
        });
        let scrollbar = self.sidebar_scrollbar(list_top, list_viewport_height, content_height, cx);
        let list = div()
            .id("work-folder-sidebar-list")
            .debug_selector(|| "sidebar-list".to_owned())
            .flex_1()
            .overflow_y_scroll()
            .track_scroll(&self.sidebar_scroll)
            .on_scroll_wheel(cx.listener(|view, _, _, cx| view.show_sidebar_scrollbar_briefly(cx)))
            .children(inline_pin_rows)
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
                        .children(pin_area)
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
        self.editor_context_menu = None;
        self.sidebar_context_menu = Some(SidebarContextMenu { position, target });
        cx.notify();
    }

    /// Whether `path` is inside (or, for `include_ancestors`, contains) the
    /// folder whose Trash move is still running.
    pub(super) fn folder_trash_blocks(&self, path: &Path, include_ancestors: bool) -> bool {
        self.folder_trash_in_flight.as_ref().is_some_and(|trashed| {
            path.starts_with(trashed) || (include_ancestors && trashed.starts_with(path))
        })
    }

    /// After `trashed` left the tree, points every unnamed draft whose save
    /// directory was inside it at the nearest ancestor that still exists in
    /// the tree (the Work Folder root at worst). Other drafts are untouched,
    /// and draft bodies are never modified.
    pub(super) fn retarget_drafts_after_folder_trash(&mut self, trashed: &Path) {
        let Some(work_folder) = self.work_folder.as_ref() else {
            return;
        };
        let root = work_folder.root().to_path_buf();
        for draft in self.work_folder_drafts.values_mut() {
            if !draft.target_directory.starts_with(trashed) {
                continue;
            }
            let replacement = trashed
                .ancestors()
                .skip(1)
                .find(|ancestor| {
                    ancestor.starts_with(&root)
                        && (*ancestor == root || work_folder.children_at(ancestor).is_some())
                })
                .map(Path::to_path_buf)
                .unwrap_or_else(|| root.clone());
            draft.target_directory = replacement;
        }
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

    /// Starts a new note inside the right-clicked folder, passing it as the
    /// explicit destination instead of changing the sidebar selection.
    pub(super) fn sidebar_context_new_note(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.sidebar_context_menu = None;
        self.new_work_folder_note_in(Some(path), cx);
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
        if self.folder_trash_in_flight.is_some() {
            self.status = Some("フォルダをゴミ箱へ移動中です。完了後にやり直してください".to_owned());
            cx.notify();
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
                        view.follow_pin_removal(&path, false, cx);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {
                        view.status = Some("ゴミ箱への移動を取り消しました".to_owned());
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
    pub(super) fn sidebar_context_trash_folder(&mut self, path: PathBuf, cx: &mut Context<Self>) {
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
        if self.folder_trash_in_flight.is_some() {
            self.status = Some("別のフォルダをゴミ箱へ移動中です。完了後にやり直してください".to_owned());
            cx.notify();
            return;
        }
        // A folder create still running under `path` would land after the OS
        // move and recreate the trashed path; refuse until it has finished.
        if self
            .pending_new_folders
            .iter()
            .any(|pending| pending.starts_with(&path))
        {
            self.status = Some(
                "フォルダを作成中のため、ゴミ箱へ移動できません。完了後にやり直してください"
                    .to_owned(),
            );
            cx.notify();
            return;
        }
        // An open read still pending under `path` would land after the OS
        // move and create a session for the trashed file; refuse until it has
        // finished.
        if self
            .loading_paths
            .iter()
            .any(|loading| loading.starts_with(&path))
        {
            self.status = Some(
                "ファイルを開いている途中のため、ゴミ箱へ移動できません。完了後にやり直してください"
                    .to_owned(),
            );
            cx.notify();
            return;
        }
        self.folder_trash_in_flight = Some(path.clone());
        self.status = Some("ゴミ箱へ移動しています…".to_owned());
        let generation = self.work_folder_generation;
        cx.spawn(async move |view, cx| {
            let trash_path = path.clone();
            let result = cx
                .background_executor()
                .spawn(async move { move_path_to_trash(&trash_path) })
                .await;
            let _ = view.update(cx, |view, cx| {
                if view.folder_trash_in_flight.as_ref() == Some(&path) {
                    view.folder_trash_in_flight = None;
                }
                if view.work_folder_generation != generation {
                    return;
                }
                match result {
                    Ok(()) => {
                        view.sessions.delete_folder(&path);
                        if let Some(folder) = view.work_folder.as_mut() {
                            folder.remove_folder(&path);
                        }
                        view.retarget_drafts_after_folder_trash(&path);
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
                        view.follow_pin_removal(&path, true, cx);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {
                        view.status = Some("ゴミ箱への移動を取り消しました".to_owned());
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
                let new_note_path = path.clone();
                items.push(self.file_tab_context_menu_item(
                    "sidebar-context-new-file",
                    "新規ファイル",
                    true,
                    cx,
                    move |view, _, _, cx| {
                        view.sidebar_context_new_note(new_note_path.clone(), cx)
                    },
                ));
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

/// A user-cancelled move: reported as `Interrupted` so callers can tell it
/// apart from an API / environment failure. It is never a success.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
fn trash_cancelled() -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::Interrupted,
        "ごみ箱への移動が取り消されました",
    )
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

/// Windows: `IFileOperation` with `FOFX_RECYCLEONDELETE` (documented as
/// "send to the Recycle Bin instead of permanently deleting"). Success also
/// requires the progress sink to observe a successful `PostDeleteItem` with a
/// Recycle Bin item (`psiNewlyCreated`); a vanished source path alone is never
/// treated as success. `PreDeleteItem` aborts any item the shell does not
/// declare as recycle-if-possible, so there is no permanent-delete fallback.
#[cfg(target_os = "windows")]
fn native_move_to_trash(path: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;

    let wide: Vec<u16> = path.as_os_str().encode_wide().collect();
    let source = shell_delete_source(&wide)?;
    // A message-pump STA owns the progress sink and keeps dispatching COM
    // messages, while a separate worker STA runs `PerformOperations`
    // synchronously. Fresh apartments also avoid `RPC_E_CHANGED_MODE` on the
    // caller (the UI input path), which never touches COM itself. See
    // `windows_recycle` and `windows_recycle_uses_separate_pump_and_worker_stas`.
    windows_recycle::recycle(&source)
}

#[cfg(target_os = "windows")]
mod windows_recycle {
    use super::{RECYCLE_OPERATION_FLAGS, RecycleReport, classify_recycle};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex, mpsc};
    use windows::Win32::Foundation::{E_ABORT, LPARAM, WPARAM};
    use windows::Win32::System::Com::{
        CLSCTX_ALL, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize,
        IStream, Marshal::CoMarshalInterThreadInterfaceInStream,
        StructuredStorage::CoGetInterfaceAndReleaseStream,
    };
    use windows::Win32::System::Threading::GetCurrentThreadId;
    use windows::Win32::UI::Shell::{
        FILEOPERATION_FLAGS, FileOperation, IFileOperation, IFileOperationProgressSink,
        IFileOperationProgressSink_Impl, IShellItem, SHCreateItemFromParsingName,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        DispatchMessageW, GetMessageW, MSG, PM_NOREMOVE, PeekMessageW, PostThreadMessageW,
        TranslateMessage, WM_QUIT, WM_USER,
    };
    use windows::core::{HRESULT, IUnknown, Interface, PCWSTR, Ref, implement};
    // `#[implement]` の展開は crate ルートの `windows_core`（Cargo.toml の直接依存。
    // `windows` 0.61 と同じ 0.61.x）を名前で参照する。

    /// `TSF_DELETE_RECYCLE_IF_POSSIBLE`
    const TSF_DELETE_RECYCLE_IF_POSSIBLE: u32 = 0x0000_0080;

    #[implement(IFileOperationProgressSink)]
    struct RecycleSink {
        report: Arc<Mutex<RecycleReport>>,
        /// Set by the worker only while it is inside `PerformOperations`.
        in_operation: Arc<AtomicBool>,
    }

    /// Carries the marshaled-interface stream (a free-threaded memory stream)
    /// from the pump STA to the worker STA.
    struct SendStream(IStream);

    // SAFETY: the stream produced by `CoMarshalInterThreadInterfaceInStream`
    // is designed to be handed to another apartment and unmarshaled there.
    unsafe impl Send for SendStream {}

    impl SendStream {
        /// Taking `self` keeps closures capturing the whole `Send` wrapper.
        fn into_inner(self) -> IStream {
            self.0
        }
    }

    /// Balances a successful `CoInitializeEx` on drop. Declare it before any
    /// COM object so it is dropped after them, including on panic.
    struct ComApartment;

    impl ComApartment {
        fn enter() -> std::io::Result<Self> {
            // SAFETY: balanced by `CoUninitialize` in `Drop` on the same thread.
            let init = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
            if init.is_err() {
                return Err(std::io::Error::other(format!(
                    "COMの初期化に失敗しました (0x{:X})",
                    init.0
                )));
            }
            Ok(Self)
        }
    }

    impl Drop for ComApartment {
        fn drop(&mut self) {
            // SAFETY: matches the successful `CoInitializeEx` in `enter`.
            unsafe { CoUninitialize() };
        }
    }

    /// What one recycle run observed, for direct API regression tests.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(super) struct Observation {
        pub pump_thread_id: u32,
        pub worker_thread_id: u32,
        pub report: RecycleReport,
    }

    impl IFileOperationProgressSink_Impl for RecycleSink_Impl {
        fn StartOperations(&self) -> windows::core::Result<()> {
            Ok(())
        }
        fn FinishOperations(&self, _hrresult: HRESULT) -> windows::core::Result<()> {
            Ok(())
        }
        fn PreRenameItem(
            &self,
            _dwflags: u32,
            _psiitem: Ref<'_, IShellItem>,
            _psznewname: &PCWSTR,
        ) -> windows::core::Result<()> {
            Ok(())
        }
        fn PostRenameItem(
            &self,
            _dwflags: u32,
            _psiitem: Ref<'_, IShellItem>,
            _psznewname: &PCWSTR,
            _hrrename: HRESULT,
            _psinewlycreated: Ref<'_, IShellItem>,
        ) -> windows::core::Result<()> {
            Ok(())
        }
        fn PreMoveItem(
            &self,
            _dwflags: u32,
            _psiitem: Ref<'_, IShellItem>,
            _psidestinationfolder: Ref<'_, IShellItem>,
            _psznewname: &PCWSTR,
        ) -> windows::core::Result<()> {
            Ok(())
        }
        fn PostMoveItem(
            &self,
            _dwflags: u32,
            _psiitem: Ref<'_, IShellItem>,
            _psidestinationfolder: Ref<'_, IShellItem>,
            _psznewname: &PCWSTR,
            _hrmove: HRESULT,
            _psinewlycreated: Ref<'_, IShellItem>,
        ) -> windows::core::Result<()> {
            Ok(())
        }
        fn PreCopyItem(
            &self,
            _dwflags: u32,
            _psiitem: Ref<'_, IShellItem>,
            _psidestinationfolder: Ref<'_, IShellItem>,
            _psznewname: &PCWSTR,
        ) -> windows::core::Result<()> {
            Ok(())
        }
        fn PostCopyItem(
            &self,
            _dwflags: u32,
            _psiitem: Ref<'_, IShellItem>,
            _psidestinationfolder: Ref<'_, IShellItem>,
            _psznewname: &PCWSTR,
            _hrcopy: HRESULT,
            _psinewlycreated: Ref<'_, IShellItem>,
        ) -> windows::core::Result<()> {
            Ok(())
        }
        fn PreDeleteItem(
            &self,
            dwflags: u32,
            _psiitem: Ref<'_, IShellItem>,
        ) -> windows::core::Result<()> {
            if dwflags & TSF_DELETE_RECYCLE_IF_POSSIBLE == 0 {
                // Not a recycle: refuse before anything is deleted.
                if let Ok(mut report) = self.report.lock() {
                    report.refused = true;
                }
                return Err(E_ABORT.into());
            }
            Ok(())
        }
        fn PostDeleteItem(
            &self,
            _dwflags: u32,
            _psiitem: Ref<'_, IShellItem>,
            hrdelete: HRESULT,
            psinewlycreated: Ref<'_, IShellItem>,
        ) -> windows::core::Result<()> {
            if let Ok(mut report) = self.report.lock() {
                // SAFETY: plain query of the calling thread's ID.
                report.callback_thread_id = Some(unsafe { GetCurrentThreadId() });
                report.callback_in_operation = Some(self.in_operation.load(Ordering::SeqCst));
                report.post_delete_seen += 1;
                if hrdelete.is_ok() && !psinewlycreated.is_null() {
                    report.recycled += 1;
                } else if hrdelete.is_err() {
                    report.failed_hresult = Some(hrdelete.0);
                }
            }
            Ok(())
        }
        fn PreNewItem(
            &self,
            _dwflags: u32,
            _psidestinationfolder: Ref<'_, IShellItem>,
            _psznewname: &PCWSTR,
        ) -> windows::core::Result<()> {
            Ok(())
        }
        fn PostNewItem(
            &self,
            _dwflags: u32,
            _psidestinationfolder: Ref<'_, IShellItem>,
            _psznewname: &PCWSTR,
            _psztemplatename: &PCWSTR,
            _dwfileattributes: u32,
            _hrnew: HRESULT,
            _psinewitem: Ref<'_, IShellItem>,
        ) -> windows::core::Result<()> {
            Ok(())
        }
        fn UpdateProgress(&self, _iworktotal: u32, _iworksofar: u32) -> windows::core::Result<()> {
            Ok(())
        }
        fn ResetTimer(&self) -> windows::core::Result<()> {
            Ok(())
        }
        fn PauseTimer(&self) -> windows::core::Result<()> {
            Ok(())
        }
        fn ResumeTimer(&self) -> windows::core::Result<()> {
            Ok(())
        }
    }

    fn hresult_error(context: &str, error: windows::core::Error) -> std::io::Error {
        std::io::Error::other(format!(
            "ごみ箱への移動に失敗しました ({context}: 0x{:X})",
            error.code().0
        ))
    }

    /// Recycles the single path in `source` (double-NUL-terminated).
    pub(super) fn recycle(source: &[u16]) -> std::io::Result<()> {
        recycle_observed(source).map(|_| ())
    }

    /// Message-pump STA: owns the sink, hands a marshaled reference to the
    /// worker and keeps dispatching COM messages until `WM_QUIT`, which is only
    /// posted after the worker has finished and released its proxy.
    fn run_pump(
        report: Arc<Mutex<RecycleReport>>,
        in_operation: Arc<AtomicBool>,
        ready: mpsc::Sender<std::io::Result<(u32, SendStream)>>,
    ) -> std::io::Result<()> {
        let apartment = match ComApartment::enter() {
            Ok(apartment) => apartment,
            Err(error) => {
                let _ = ready.send(Err(error));
                return Ok(());
            }
        };
        let setup = || -> std::io::Result<(IFileOperationProgressSink, SendStream)> {
            let sink: IFileOperationProgressSink = RecycleSink {
                report,
                in_operation,
            }
            .into();
            let unknown: IUnknown = sink
                .cast()
                .map_err(|e| hresult_error("QueryInterface", e))?;
            // SAFETY: `unknown` is a valid interface of the object whose IID is given.
            let stream = unsafe {
                CoMarshalInterThreadInterfaceInStream(&IFileOperationProgressSink::IID, &unknown)
            }
            .map_err(|e| hresult_error("CoMarshalInterThreadInterfaceInStream", e))?;
            Ok((sink, SendStream(stream)))
        };
        let (sink, stream) = match setup() {
            Ok(pair) => pair,
            Err(error) => {
                let _ = ready.send(Err(error));
                drop(apartment);
                return Ok(());
            }
        };
        // Create this thread's message queue before announcing the thread ID,
        // so a later `PostThreadMessageW(WM_QUIT)` cannot be lost.
        let mut msg = MSG::default();
        // SAFETY: `msg` is a valid out-pointer.
        let _ = unsafe { PeekMessageW(&mut msg, None, WM_USER, WM_USER, PM_NOREMOVE) };
        // SAFETY: plain query of the calling thread's ID.
        let thread_id = unsafe { GetCurrentThreadId() };
        if ready.send(Ok((thread_id, stream))).is_err() {
            drop(sink);
            drop(apartment);
            return Ok(());
        }
        let mut result = Ok(());
        loop {
            // SAFETY: `msg` is a valid out-pointer; standard message loop.
            let status = unsafe { GetMessageW(&mut msg, None, 0, 0) };
            if status.0 == 0 {
                break;
            }
            if status.0 < 0 {
                result = Err(std::io::Error::other(
                    "ごみ箱処理のメッセージループが失敗しました",
                ));
                break;
            }
            // SAFETY: `msg` was filled in by `GetMessageW`.
            unsafe {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        // Release the sink before uninitializing this apartment.
        drop(sink);
        drop(apartment);
        result
    }

    /// Worker STA: unmarshals the sink, advises and runs `PerformOperations`
    /// synchronously. Returns its thread ID on success.
    fn run_worker(
        source: Vec<u16>,
        stream: SendStream,
        in_operation: Arc<AtomicBool>,
        report: Arc<Mutex<RecycleReport>>,
    ) -> std::io::Result<u32> {
        // Declared first so it is dropped last, after every COM object.
        let _apartment = ComApartment::enter()?;
        // SAFETY: plain query of the calling thread's ID.
        let thread_id = unsafe { GetCurrentThreadId() };
        let stream = stream.into_inner();
        // SAFETY: the call releases the stream itself (even on failure), so
        // the wrapper is forgotten below to avoid a double release.
        let sink: windows::core::Result<IFileOperationProgressSink> =
            unsafe { CoGetInterfaceAndReleaseStream(&stream) };
        std::mem::forget(stream);
        let sink = sink.map_err(|e| hresult_error("CoGetInterfaceAndReleaseStream", e))?;
        recycle_in_worker(&source, &sink, &in_operation, &report)?;
        Ok(thread_id)
    }

    /// Starts the pump and worker STAs, waits for the worker, then shuts the
    /// pump down. Both threads are always joined before returning.
    pub(super) fn recycle_observed(source: &[u16]) -> std::io::Result<Observation> {
        let report = Arc::new(Mutex::new(RecycleReport::default()));
        let in_operation = Arc::new(AtomicBool::new(false));
        let (ready_tx, ready_rx) = mpsc::channel();
        let pump = std::thread::Builder::new()
            .name("hane-recycle-pump".to_owned())
            .spawn({
                let report = Arc::clone(&report);
                let in_operation = Arc::clone(&in_operation);
                move || run_pump(report, in_operation, ready_tx)
            })?;
        let (pump_thread_id, stream) = match ready_rx.recv() {
            Ok(Ok(ready)) => ready,
            Ok(Err(error)) => {
                let _ = pump.join();
                return Err(error);
            }
            Err(_) => {
                let _ = pump.join();
                return Err(std::io::Error::other(
                    "ごみ箱処理のメッセージポンプを開始できませんでした",
                ));
            }
        };
        let worker_result = std::thread::Builder::new()
            .name("hane-recycle-worker".to_owned())
            .spawn({
                let report = Arc::clone(&report);
                let in_operation = Arc::clone(&in_operation);
                let source = source.to_vec();
                move || run_worker(source, stream, in_operation, report)
            })
            .and_then(|worker| {
                worker.join().unwrap_or_else(|_| {
                    Err(std::io::Error::other("ごみ箱への移動処理が異常終了しました"))
                })
            });
        // The worker has finished and released its proxy; stop the pump.
        // SAFETY: the pump created its message queue before announcing its ID.
        let posted = unsafe { PostThreadMessageW(pump_thread_id, WM_QUIT, WPARAM(0), LPARAM(0)) };
        if posted.is_err() {
            // Joining would block forever on a pump that never sees WM_QUIT.
            return Err(std::io::Error::other(
                "ごみ箱処理のメッセージポンプを停止できませんでした",
            ));
        }
        let pump_result = pump
            .join()
            .unwrap_or_else(|_| Err(std::io::Error::other("ごみ箱処理のメッセージポンプが異常終了しました")));
        let worker_thread_id = worker_result?;
        pump_result?;
        let report = report.lock().map(|r| r.clone()).unwrap_or_default();
        Ok(Observation {
            pump_thread_id,
            worker_thread_id,
            report,
        })
    }

    fn recycle_in_worker(
        source: &[u16],
        sink: &IFileOperationProgressSink,
        in_operation: &AtomicBool,
        report: &Mutex<RecycleReport>,
    ) -> std::io::Result<()> {
        // SAFETY: `source` is NUL-terminated and outlives the call.
        let item: IShellItem = unsafe {
            SHCreateItemFromParsingName(PCWSTR(source.as_ptr()), None::<&windows::Win32::System::Com::IBindCtx>)
        }
        .map_err(|e| hresult_error("SHCreateItemFromParsingName", e))?;
        // SAFETY: plain COM activation in the STA initialized by the caller.
        let operation: IFileOperation = unsafe { CoCreateInstance(&FileOperation, None, CLSCTX_ALL) }
            .map_err(|e| hresult_error("CoCreateInstance", e))?;
        // SAFETY: all interface pointers are valid for the duration of the calls.
        let (performed, aborted) = unsafe {
            operation
                .SetOperationFlags(FILEOPERATION_FLAGS(RECYCLE_OPERATION_FLAGS))
                .map_err(|e| hresult_error("SetOperationFlags", e))?;
            let cookie = operation
                .Advise(sink)
                .map_err(|e| hresult_error("Advise", e))?;
            let queued = operation
                .DeleteItem(&item, None::<&IFileOperationProgressSink>)
                .map_err(|e| hresult_error("DeleteItem", e));
            let performed = match queued {
                Ok(()) => {
                    // The pump STA dispatches sink callbacks while this blocks.
                    in_operation.store(true, Ordering::SeqCst);
                    let performed = operation.PerformOperations().map_err(|e| e.code().0);
                    in_operation.store(false, Ordering::SeqCst);
                    performed
                }
                Err(error) => {
                    let _ = operation.Unadvise(cookie);
                    return Err(error);
                }
            };
            let aborted = operation
                .GetAnyOperationsAborted()
                .map(|b| b.as_bool())
                .unwrap_or(true);
            let _ = operation.Unadvise(cookie);
            (performed, aborted)
        };
        drop(operation);
        drop(item);
        let snapshot = report.lock().map(|r| r.clone()).unwrap_or_default();
        classify_recycle(performed, aborted, &snapshot)
    }
}

/// `FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_NOERRORUI | FOF_SILENT |
/// FOF_WANTNUKEWARNING | FOFX_RECYCLEONDELETE`. `FOFX_RECYCLEONDELETE` sends
/// to the Recycle Bin instead of permanently deleting; `FOF_WANTNUKEWARNING`
/// partially overrides `FOF_NOCONFIRMATION` so an item that cannot be
/// recycled asks the user (who can cancel) instead of vanishing silently.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
const RECYCLE_OPERATION_FLAGS: u32 = 0x0040 | 0x0010 | 0x0400 | 0x0004 | 0x4000 | 0x0008_0000;

/// What the `IFileOperation` progress sink observed.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct RecycleReport {
    /// `PostDeleteItem` calls seen.
    post_delete_seen: u32,
    /// Of those, successes that reported a Recycle Bin item.
    recycled: u32,
    /// HRESULT of a failed delete, if any.
    failed_hresult: Option<i32>,
    /// `PreDeleteItem` refused a non-recycle delete.
    refused: bool,
    /// Thread that ran the last `PostDeleteItem` callback.
    callback_thread_id: Option<u32>,
    /// Whether that callback arrived while the worker was inside
    /// `PerformOperations`.
    callback_in_operation: Option<bool>,
}

/// Maps `PerformOperations`' result (`Err` carries the HRESULT), the aborted
/// flag and the sink report to success, user cancel (`Interrupted`) or
/// failure. Success requires exactly one item observed as recycled.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
fn classify_recycle(
    performed: Result<(), i32>,
    any_aborted: bool,
    report: &RecycleReport,
) -> std::io::Result<()> {
    const HRESULT_USER_CANCELLED: i32 = 0x8007_04C7_u32 as i32;
    const COPYENGINE_E_USER_CANCELLED: i32 = 0x8027_0000_u32 as i32;
    if any_aborted && !report.refused {
        return Err(trash_cancelled());
    }
    if let Err(hr) = performed {
        if hr == HRESULT_USER_CANCELLED || hr == COPYENGINE_E_USER_CANCELLED {
            return Err(trash_cancelled());
        }
        return Err(std::io::Error::other(format!(
            "ごみ箱への移動に失敗しました (IFileOperation: 0x{:X})",
            hr as u32
        )));
    }
    if report.refused {
        return Err(std::io::Error::other(
            "ごみ箱へ移動できないため、削除を中止しました",
        ));
    }
    if let Some(hr) = report.failed_hresult {
        return Err(std::io::Error::other(format!(
            "ごみ箱への移動に失敗しました (IFileOperation: 0x{:X})",
            hr as u32
        )));
    }
    if report.post_delete_seen == 1 && report.recycled == 1 {
        Ok(())
    } else {
        Err(std::io::Error::other(
            "ごみ箱への移動を確認できなかったため、成功として扱いません",
        ))
    }
}

/// Builds the double-NUL-terminated `pFrom` list for one path. An interior
/// NUL would silently turn into a second path, so it is rejected.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
fn shell_delete_source(path: &[u16]) -> std::io::Result<Vec<u16>> {
    if path.contains(&0) {
        return Err(std::io::Error::other(
            "パスにNUL文字が含まれているため、ごみ箱へ移動できません",
        ));
    }
    let mut wide = path.to_vec();
    wide.extend([0, 0]);
    Ok(wide)
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

    fn recycled_report() -> RecycleReport {
        RecycleReport {
            post_delete_seen: 1,
            recycled: 1,
            ..RecycleReport::default()
        }
    }

    #[test]
    fn recycle_result_requires_observed_recycle_bin_item() {
        assert!(classify_recycle(Ok(()), false, &recycled_report()).is_ok());
        // API success with no observed recycle (e.g. permanent delete) fails.
        let permanent = RecycleReport {
            post_delete_seen: 1,
            ..RecycleReport::default()
        };
        let error = classify_recycle(Ok(()), false, &permanent).unwrap_err();
        assert_ne!(error.kind(), std::io::ErrorKind::Interrupted);
        // No callback at all is not a success either.
        assert!(classify_recycle(Ok(()), false, &RecycleReport::default()).is_err());
        // More than one deleted item is unexpected for a single path.
        let many = RecycleReport {
            post_delete_seen: 2,
            recycled: 2,
            ..RecycleReport::default()
        };
        assert!(classify_recycle(Ok(()), false, &many).is_err());
    }

    #[test]
    fn recycle_result_distinguishes_cancel_refusal_and_error() {
        for hr in [0x8007_04C7_u32 as i32, 0x8027_0000_u32 as i32] {
            let cancelled = classify_recycle(Err(hr), false, &RecycleReport::default()).unwrap_err();
            assert_eq!(cancelled.kind(), std::io::ErrorKind::Interrupted);
        }
        // Aborted flag wins over an otherwise complete report.
        let aborted = classify_recycle(Ok(()), true, &recycled_report()).unwrap_err();
        assert_eq!(aborted.kind(), std::io::ErrorKind::Interrupted);
        let failed = classify_recycle(Err(0x8000_4005_u32 as i32), false, &RecycleReport::default())
            .unwrap_err();
        assert_ne!(failed.kind(), std::io::ErrorKind::Interrupted);
        let refused = RecycleReport {
            refused: true,
            ..RecycleReport::default()
        };
        // PreDeleteItem refusal aborts the operation but is an error, not a cancel.
        let error = classify_recycle(Ok(()), true, &refused).unwrap_err();
        assert_ne!(error.kind(), std::io::ErrorKind::Interrupted);
        let item_failed = RecycleReport {
            post_delete_seen: 1,
            failed_hresult: Some(0x8007_0005_u32 as i32),
            ..RecycleReport::default()
        };
        assert!(classify_recycle(Ok(()), false, &item_failed).is_err());
    }

    #[test]
    fn recycle_flags_request_recycle_bin_without_permanent_delete_fallback() {
        // FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_NOERRORUI | FOF_SILENT
        // | FOF_WANTNUKEWARNING | FOFX_RECYCLEONDELETE
        assert_eq!(
            RECYCLE_OPERATION_FLAGS,
            0x0040 | 0x0010 | 0x0400 | 0x0004 | 0x4000 | 0x0008_0000
        );
        assert_ne!(RECYCLE_OPERATION_FLAGS & 0x0008_0000, 0);
        assert_ne!(RECYCLE_OPERATION_FLAGS & 0x0040, 0);
        assert_ne!(RECYCLE_OPERATION_FLAGS & 0x4000, 0);
    }

    /// Directly exercises the real API: the pump STA and the worker STA are
    /// separate threads, `PostDeleteItem` arrives while the worker is still
    /// inside `PerformOperations`, and it reports a Recycle Bin item. The
    /// callback's thread ID is not part of the public contract, so it is not
    /// asserted.
    #[cfg(target_os = "windows")]
    #[test]
    fn windows_recycle_uses_separate_pump_and_worker_stas() {
        use std::os::windows::ffi::OsStrExt;

        let dir = trash_test_dir("windows-recycle");
        let folder = dir.join("folder");
        std::fs::create_dir(&folder).unwrap();
        std::fs::write(folder.join("note.md"), "x").unwrap();
        let wide: Vec<u16> = folder.as_os_str().encode_wide().collect();
        let source = shell_delete_source(&wide).unwrap();
        let observed = windows_recycle::recycle_observed(&source).unwrap();
        assert_ne!(observed.pump_thread_id, observed.worker_thread_id);
        assert!(observed.report.callback_thread_id.is_some());
        assert_eq!(observed.report.callback_in_operation, Some(true));
        assert_eq!(observed.report.post_delete_seen, 1);
        assert_eq!(observed.report.recycled, 1);
        assert!(!observed.report.refused);
        assert!(!folder.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_native_move_to_trash_recycles_folder() {
        let dir = trash_test_dir("windows-native-trash");
        let folder = dir.join("folder");
        std::fs::create_dir(&folder).unwrap();
        std::fs::write(folder.join("note.md"), "x").unwrap();
        native_move_to_trash(&folder).unwrap();
        assert!(!folder.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn shell_delete_source_is_double_nul_terminated_and_rejects_interior_nul() {
        let wide = shell_delete_source(&['a' as u16, 'b' as u16]).unwrap();
        assert_eq!(wide, ['a' as u16, 'b' as u16, 0, 0]);
        let error = shell_delete_source(&['a' as u16, 0, 'b' as u16]).unwrap_err();
        assert_ne!(error.kind(), std::io::ErrorKind::Interrupted);
    }

    #[test]
    fn cancelled_trash_leaves_path_and_is_not_success() {
        let dir = trash_test_dir("cancelled");
        let path = dir.join("note.md");
        std::fs::write(&path, "x").unwrap();
        let result = move_path_to_trash_with(&path, |_| Err(trash_cancelled()));
        assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::Interrupted);
        assert!(path.exists());
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

    /// Clicks a row's hover "new folder" / "new file" button through the real
    /// GPUI event path. `directory` is the root when `None`; otherwise the row
    /// is found in the flattened tree, in the order the sidebar renders it.
    fn click_create_button(
        view: &gpui::Entity<EditorView>,
        cx: &mut gpui::VisualTestContext,
        directory: Option<&Path>,
        note: bool,
    ) {
        cx.run_until_parked();
        let row_height = px(SIDEBAR_ROW_HEIGHT);
        let root_row = cx.debug_bounds("sidebar-root").expect("the root row");
        let point = match directory {
            None => {
                cx.simulate_mouse_move(root_row.center(), None, gpui::Modifiers::none());
                cx.run_until_parked();
                // The root row has no pin slot: its create slot is the row's
                // right-most element, the folder button first, the note second.
                let slot_width = sidebar_pins::PIN_SLOT_SIZE;
                let slot_right = root_row.right() - px(SIDEBAR_ROW_HORIZONTAL_PADDING);
                let button_center_x = if note {
                    slot_right - px(slot_width / 2.0)
                } else {
                    slot_right - px(slot_width * 1.5)
                };
                gpui::point(button_center_x, root_row.center().y)
            }
            Some(directory) => {
                let target_index = view.read_with(cx, |view, _| {
                    let mut rows = Vec::new();
                    let query = view.sidebar_filter.text().to_lowercase();
                    if query.is_empty() {
                        flatten_work_folder_tree(
                            view.work_folder.as_ref().unwrap().children(),
                            1,
                            &view.expanded_folders,
                            &mut rows,
                        );
                    } else {
                        flatten_filtered_work_folder_tree(
                            view.work_folder.as_ref().unwrap().children(),
                            1,
                            &query,
                            &mut rows,
                        );
                    }
                    rows.iter()
                        .position(|row| {
                            matches!(row.node, WorkFolderNode::Folder(folder) if folder.path() == directory)
                        })
                        .expect("the folder row is visible")
                        + 1
                });
                let selector = if note {
                    "sidebar-folder-create-note"
                } else {
                    "sidebar-folder-create-folder"
                };
                // The buttons only exist while their row is hovered: move onto
                // the wanted row first so the reported bounds are its button.
                let row_center = gpui::point(
                    root_row.center().x,
                    root_row.center().y + row_height * target_index as f32,
                );
                cx.simulate_mouse_move(row_center, None, gpui::Modifiers::none());
                cx.run_until_parked();
                let button = cx.debug_bounds(selector).expect("a folder create button");
                assert!(
                    button.top() <= row_center.y && row_center.y <= button.bottom(),
                    "the hovered row's button is the one reported"
                );
                button.center()
            }
        };
        cx.simulate_click(point, gpui::Modifiers::none());
        cx.run_until_parked();
    }

    // Issue #642: the real hover buttons create in their own row's folder,
    // whatever is selected, and never reach the row's select / toggle / rename
    // / open handlers.
    #[gpui::test]
    fn row_create_buttons_create_in_their_own_folder_without_triggering_the_row(
        cx: &mut gpui::TestAppContext,
    ) {
        let root = std::env::temp_dir().join(format!(
            "hane-row-create-buttons-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("a/inner")).unwrap();
        std::fs::create_dir_all(root.join("b")).unwrap();
        std::fs::write(root.join("a/Existing.md"), "# Existing\n").unwrap();
        let work_folder = OsWorkFolderScanner.scan(&root).unwrap();

        cx.update(crate::actions::register_key_bindings);
        let (view, cx) = cx.add_window_view(|_, cx| {
            EditorView::from_sessions(
                SessionSet::with_untitled("", "Untitled"),
                Arc::new(OsFileService),
                StateStores::memory(),
                cx,
            )
        });
        cx.simulate_resize(gpui::size(px(960.0), px(760.0)));
        view.update(cx, |view, cx| {
            view.work_folder = Some(work_folder);
            // `a` is expanded and `inner` collapsed; `b` is the selection.
            view.expanded_folders.insert(root.join("a"));
            view.selected_folder = Some(root.join("b"));
            view.sidebar_focus = SidebarFocus::Folder;
            cx.notify();
        });
        cx.run_until_parked();

        let assert_row_untouched = |view: &gpui::Entity<EditorView>,
                                    cx: &mut gpui::VisualTestContext,
                                    inner_expanded: bool| {
            view.read_with(cx, |view, _| {
                assert_eq!(view.selected_folder, Some(root.join("b")));
                assert!(view.expanded_folders.contains(&root.join("a")));
                assert_eq!(
                    view.expanded_folders.contains(&root.join("a/inner")),
                    inner_expanded
                );
                assert!(view.inline_rename.is_none(), "no rename starts");
                assert!(view.loading_paths.is_empty(), "no file opens");
            });
        };

        // Nested parent row: two folders in a row, then a note.
        let inner = root.join("a/inner");
        click_create_button(&view, cx, Some(&inner), false);
        click_create_button(&view, cx, Some(&inner), false);
        assert!(root.join("a/inner/New Folder").is_dir());
        assert!(root.join("a/inner/New Folder 2").is_dir());
        assert!(!root.join("b/New Folder").exists());
        // Only the creation itself expands `inner`; the row click would have
        // toggled it back closed on the second press.
        assert_row_untouched(&view, cx, true);

        click_create_button(&view, cx, Some(&inner), true);
        view.read_with(cx, |view, _| {
            assert_eq!(view.work_folder_drafts.len(), 1);
            assert!(
                view.work_folder_drafts
                    .values()
                    .all(|draft| draft.target_directory == inner),
                "the note starts in the clicked row's folder"
            );
        });

        // The outer parent row.
        let outer = root.join("a");
        click_create_button(&view, cx, Some(&outer), false);
        assert!(root.join("a/New Folder").is_dir());
        click_create_button(&view, cx, Some(&outer), true);
        view.read_with(cx, |view, _| {
            assert_eq!(view.work_folder_drafts.len(), 2);
            assert!(
                view.work_folder_drafts
                    .values()
                    .any(|draft| draft.target_directory == outer),
                "the note starts in the outer row's folder"
            );
        });
        assert_row_untouched(&view, cx, true);

        // The root row.
        click_create_button(&view, cx, None, false);
        assert!(root.join("New Folder").is_dir());
        assert!(!root.join("b/New Folder").exists());
        click_create_button(&view, cx, None, true);
        view.read_with(cx, |view, _| {
            assert_eq!(view.work_folder_drafts.len(), 3);
            assert!(
                view.work_folder_drafts
                    .values()
                    .any(|draft| draft.target_directory == root),
                "the note starts in the root"
            );
        });
        assert_row_untouched(&view, cx, true);

        std::fs::remove_dir_all(&root).unwrap();
    }

    // Issue #642: in a 13-level-deep row with a long Japanese name at
    // `SIDEBAR_MIN_WIDTH`, the hover create / pin buttons stay inside the row
    // and the sidebar viewport, never overlap, and the real clicks at their
    // centres still reach their own row's handlers.
    #[gpui::test]
    fn deep_row_buttons_stay_reachable_at_minimum_sidebar_width(cx: &mut gpui::TestAppContext) {
        let root = std::env::temp_dir().join(format!(
            "hane-row-create-deep-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let mut target = root.clone();
        let mut ancestors = Vec::new();
        for level in 1..=12 {
            target = target.join(format!("階層{level}"));
            ancestors.push(target.clone());
        }
        let target = target.join("とても長い日本語のフォルダ名".repeat(3));
        std::fs::create_dir_all(&target).unwrap();
        std::fs::create_dir_all(root.join("other")).unwrap();
        let work_folder = OsWorkFolderScanner.scan(&root).unwrap();

        cx.update(crate::actions::register_key_bindings);
        let (view, cx) = cx.add_window_view(|_, cx| {
            EditorView::from_sessions(
                SessionSet::with_untitled("", "Untitled"),
                Arc::new(OsFileService),
                StateStores::memory(),
                cx,
            )
        });
        cx.simulate_resize(gpui::size(px(960.0), px(900.0)));
        view.update(cx, |view, cx| {
            view.work_folder = Some(work_folder);
            view.begin_pin_load(cx);
            view.sidebar_width = SIDEBAR_MIN_WIDTH;
            for ancestor in &ancestors {
                view.expanded_folders.insert(ancestor.clone());
            }
            view.selected_folder = Some(root.join("other"));
            view.sidebar_focus = SidebarFocus::Folder;
            cx.notify();
        });
        cx.run_until_parked();

        // Hovers the target row and returns (row, new folder, new file, pin)
        // bounds, after asserting they are all reachable.
        let measure_calls = std::cell::Cell::new(0usize);
        let measure = |view: &gpui::Entity<EditorView>,
                       cx: &mut gpui::VisualTestContext|
         -> [gpui::Bounds<gpui::Pixels>; 4] {
            measure_calls.set(measure_calls.get() + 1);
            let call = measure_calls.get();
            cx.run_until_parked();
            let (index, depth) = view.read_with(cx, |view, _| {
                let mut rows = Vec::new();
                flatten_work_folder_tree(
                    view.work_folder.as_ref().unwrap().children(),
                    1,
                    &view.expanded_folders,
                    &mut rows,
                );
                let position = rows
                    .iter()
                    .position(|row| {
                        matches!(row.node, WorkFolderNode::Folder(folder) if folder.path() == target)
                    })
                    .expect("the deep folder row is visible");
                (position + 1, rows[position].depth)
            });
            assert_eq!(depth, 13, "the target is 13 levels deep");
            let root_row = cx.debug_bounds("sidebar-root").expect("the root row");
            let list = cx.debug_bounds("sidebar-list").expect("the sidebar list");
            assert!(
                list.right() <= px(SIDEBAR_MIN_WIDTH),
                "the sidebar is at its minimum width: {list:?}"
            );
            let row = gpui::Bounds::new(
                gpui::point(
                    root_row.left(),
                    root_row.top() + px(SIDEBAR_ROW_HEIGHT) * index as f32,
                ),
                gpui::size(root_row.size.width, px(SIDEBAR_ROW_HEIGHT)),
            );
            assert!(
                row.top() >= list.top() && row.bottom() <= list.bottom(),
                "the target row is inside the viewport: {row:?} in {list:?}"
            );
            cx.simulate_mouse_move(row.center(), None, gpui::Modifiers::none());
            cx.run_until_parked();
            // The selectors below repeat on every row and `debug_bounds`
            // returns one of them, so also record the target row's own
            // create slot (keyed by its flatten index) as a reference.
            // `debug_bounds` requires `&'static str`; the selector is used a
            // bounded number of times in this test, so leaking is acceptable.
            let slot_selector: &'static str = Box::leak(
                format!(
                    "sidebar-folder-create-slot:work-folder-create:{}",
                    index - 1
                )
                .into_boxed_str(),
            );
            // The pin button is likewise keyed by the target's own row (an
            // ordinary tree row, never a Pinned-area row), because the shared
            // `sidebar-pin-toggle` key is overwritten by neighbouring rows.
            let pin_selector: &'static str = Box::leak(
                format!(
                    "sidebar-pin-toggle-row:work-folder-pin-toggle:{}",
                    index - 1
                )
                .into_boxed_str(),
            );
            let snapshot = |cx: &mut gpui::VisualTestContext| {
                (
                    cx.debug_bounds("sidebar-folder-create-folder"),
                    cx.debug_bounds("sidebar-folder-create-note"),
                    cx.debug_bounds(pin_selector),
                    cx.debug_bounds(slot_selector),
                )
            };
            let first = snapshot(cx);
            // One more frame at the same hover point, to see whether a
            // vertical offset is a stale frame.
            cx.simulate_mouse_move(row.center(), None, gpui::Modifiers::none());
            cx.run_until_parked();
            let second = snapshot(cx);
            let diagnostics = format!(
                "measure call #{call}: target={target:?} depth={depth} flatten_index={} \
                 estimated_row={row:?} sidebar-root={root_row:?} sidebar-list={list:?} \
                 sidebar-pin-area={:?} hover={:?}; \
                 [first frame] folder_button={:?} note_button={:?} pin_button={:?} \
                 target_row_slot={:?}; \
                 [after one more frame] folder_button={:?} note_button={:?} pin_button={:?} \
                 target_row_slot={:?}",
                index - 1,
                cx.debug_bounds("sidebar-pin-area"),
                row.center(),
                first.0,
                first.1,
                first.2,
                first.3,
                second.0,
                second.1,
                second.2,
                second.3,
            );
            let folder_button = first
                .0
                .unwrap_or_else(|| panic!("the new folder button is rendered: {diagnostics}"));
            let note_button = first
                .1
                .unwrap_or_else(|| panic!("the new file button is rendered: {diagnostics}"));
            let pin_button = first
                .2
                .unwrap_or_else(|| panic!("the pin button is rendered: {diagnostics}"));
            let buttons = [folder_button, note_button, pin_button];
            for button in buttons {
                assert!(
                    button.left() >= row.left()
                        && button.right() <= row.right()
                        && button.top() >= row.top()
                        && button.bottom() <= row.bottom(),
                    "{button:?} stays inside the row {row:?}: {diagnostics}"
                );
                assert!(
                    button.left() >= list.left()
                        && button.right() <= list.right()
                        && button.top() >= list.top()
                        && button.bottom() <= list.bottom(),
                    "{button:?} stays inside the sidebar viewport {list:?}: {diagnostics}"
                );
                let center = button.center();
                assert!(
                    center.x >= list.left()
                        && center.x <= list.right()
                        && center.y >= list.top()
                        && center.y <= list.bottom(),
                    "the click point {center:?} is not clipped: {diagnostics}"
                );
            }
            for (index, first) in buttons.iter().enumerate() {
                for second in &buttons[index + 1..] {
                    assert!(
                        first.right() <= second.left() || second.right() <= first.left(),
                        "{first:?} and {second:?} must not overlap: {diagnostics}"
                    );
                }
            }
            [row, folder_button, note_button, pin_button]
        };
        let assert_row_untouched = |view: &gpui::Entity<EditorView>,
                                    cx: &mut gpui::VisualTestContext| {
            view.read_with(cx, |view, _| {
                assert_eq!(view.selected_folder, Some(root.join("other")));
                assert!(view.inline_rename.is_none(), "no rename starts");
                assert!(view.loading_paths.is_empty(), "no file opens");
            });
        };

        // Two folders in a row: a propagated row click would toggle the
        // target closed again on the second press.
        for _ in 0..2 {
            let [_, folder_button, _, _] = measure(&view, cx);
            cx.simulate_click(folder_button.center(), gpui::Modifiers::none());
            cx.run_until_parked();
        }
        assert!(target.join("New Folder").is_dir());
        assert!(target.join("New Folder 2").is_dir());
        assert!(!root.join("other/New Folder").exists());
        view.read_with(cx, |view, _| {
            assert!(view.expanded_folders.contains(&target));
        });
        assert_row_untouched(&view, cx);

        let [_, _, note_button, _] = measure(&view, cx);
        cx.simulate_click(note_button.center(), gpui::Modifiers::none());
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert_eq!(view.work_folder_drafts.len(), 1);
            assert!(
                view.work_folder_drafts
                    .values()
                    .all(|draft| draft.target_directory == target),
                "the note starts in the deep row's folder"
            );
        });
        assert_row_untouched(&view, cx);

        // Pin through the real click; the Pinned area shifts the rows.
        let pinned_here = |view: &gpui::Entity<EditorView>,
                           cx: &mut gpui::VisualTestContext| {
            view.read_with(cx, |view, _| {
                view.sidebar_pins
                    .entries
                    .iter()
                    .any(|entry| root.join(&entry.relative_path) == target)
            })
        };
        let [_, _, _, pin_button] = measure(&view, cx);
        cx.simulate_click(pin_button.center(), gpui::Modifiers::none());
        cx.run_until_parked();
        assert!(pinned_here(&view, cx), "the deep folder is pinned");
        assert_row_untouched(&view, cx);

        // The remaining create buttons are still reachable after the shift.
        let [_, folder_button, _, _] = measure(&view, cx);
        cx.simulate_click(folder_button.center(), gpui::Modifiers::none());
        cx.run_until_parked();
        assert!(target.join("New Folder 3").is_dir());
        assert!(!root.join("other/New Folder 3").exists());
        view.read_with(cx, |view, _| {
            assert!(view.expanded_folders.contains(&target));
        });
        assert_row_untouched(&view, cx);

        // Unpin from the same ordinary row.
        let [_, _, _, pin_button] = measure(&view, cx);
        cx.simulate_click(pin_button.center(), gpui::Modifiers::none());
        cx.run_until_parked();
        assert!(!pinned_here(&view, cx), "the deep folder is unpinned");
        assert_row_untouched(&view, cx);
        let [_, _, note_button, _] = measure(&view, cx);
        cx.simulate_click(note_button.center(), gpui::Modifiers::none());
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert_eq!(view.work_folder_drafts.len(), 2);
        });

        std::fs::remove_dir_all(&root).unwrap();
    }

    // Issue #642: a folder created from a row's hover button while the filter
    // shows that row through closed ancestors stays hidden (it is empty) during
    // the filter, and every ancestor plus the new folder is rendered and
    // reachable once the filter is cleared through the real input path.
    #[gpui::test]
    fn folder_created_under_a_filtered_closed_ancestor_chain_is_reachable_after_clearing_the_filter(
        cx: &mut gpui::TestAppContext,
    ) {
        let root = std::env::temp_dir().join(format!(
            "hane-row-create-filtered-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let level1 = root.join("a");
        let level2 = level1.join("b");
        let level3 = level2.join("c");
        let target = level3.join("target");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::create_dir_all(root.join("other")).unwrap();
        std::fs::write(target.join("Needle.md"), "# Needle\n").unwrap();
        let work_folder = OsWorkFolderScanner.scan(&root).unwrap();

        cx.update(crate::actions::register_key_bindings);
        let (view, cx) = cx.add_window_view(|_, cx| {
            EditorView::from_sessions(
                SessionSet::with_untitled("", "Untitled"),
                Arc::new(OsFileService),
                StateStores::memory(),
                cx,
            )
        });
        cx.simulate_resize(gpui::size(px(960.0), px(760.0)));
        view.update(cx, |view, cx| {
            view.work_folder = Some(work_folder);
            // Every ancestor of `target` is closed; another folder is selected.
            view.selected_folder = Some(root.join("other"));
            view.sidebar_focus = SidebarFocus::Folder;
            cx.notify();
        });
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert!(view.expanded_folders.is_empty());
        });

        // Filter through the real input path.
        let filter_point = cx.debug_bounds("sidebar-filter").unwrap().center();
        cx.simulate_click(filter_point, gpui::Modifiers::none());
        cx.simulate_input("needle");
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert_eq!(view.sidebar_filter.text(), "needle");
            assert!(view.expanded_folders.is_empty());
        });
        assert!(cx.debug_bounds("sidebar-filter-empty").is_none());

        // Hover the filtered target row and click its real new-folder button.
        click_create_button(&view, cx, Some(&target), false);
        let created = target.join("New Folder");
        assert!(created.is_dir(), "created inside the hovered folder");
        assert!(!root.join("other/New Folder").exists());
        assert!(!level3.join("New Folder").exists());
        view.read_with(cx, |view, _| {
            assert_eq!(view.selected_folder, Some(root.join("other")));
            assert!(view.inline_rename.is_none(), "no rename starts");
            assert!(view.loading_paths.is_empty(), "no file opens");
            let mut rows = Vec::new();
            flatten_filtered_work_folder_tree(
                view.work_folder.as_ref().unwrap().children(),
                1,
                "needle",
                &mut rows,
            );
            let paths: Vec<&Path> = rows.iter().map(|row| row.node.path()).collect();
            assert_eq!(
                paths,
                vec![
                    level1.as_path(),
                    level2.as_path(),
                    level3.as_path(),
                    target.as_path(),
                    target.join("Needle.md").as_path(),
                ],
                "the empty new folder stays hidden while filtering"
            );
        });

        // Creating the folder blurred the filter; click the currently
        // rendered filter again to bring input focus back.
        let filter_point = cx.debug_bounds("sidebar-filter").unwrap().center();
        cx.simulate_click(filter_point, gpui::Modifiers::none());
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert!(
                view.sidebar_filter_focused,
                "clicking the filter restores input focus"
            );
            assert_eq!(view.sidebar_filter.text(), "needle");
        });

        // Clear the filter through the real input path.
        cx.simulate_keystrokes(if cfg!(target_os = "macos") {
            "cmd-a"
        } else {
            "ctrl-a"
        });
        cx.simulate_keystrokes("backspace");
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert!(view.sidebar_filter.text().is_empty());
        });
        assert!(cx.debug_bounds("sidebar-filter-empty").is_none());

        // Every ancestor and the new folder are rendered rows that can be
        // hovered and reached: the real hover button is reported on each row.
        for folder in [&level1, &level2, &level3, &target, &created] {
            click_create_button_hover_only(&view, cx, folder);
        }
        // The new folder row accepts a real click on its own button.
        click_create_button(&view, cx, Some(&created), true);
        view.read_with(cx, |view, _| {
            assert!(
                view.work_folder_drafts
                    .values()
                    .any(|draft| draft.target_directory == created),
                "the note starts in the new folder"
            );
            assert_eq!(view.selected_folder, Some(root.join("other")));
        });

        std::fs::remove_dir_all(&root).unwrap();
    }

    /// Hovers the rendered row of `directory` in the unfiltered tree and
    /// asserts that its create button is rendered on that row.
    fn click_create_button_hover_only(
        view: &gpui::Entity<EditorView>,
        cx: &mut gpui::VisualTestContext,
        directory: &Path,
    ) {
        cx.run_until_parked();
        let root_row = cx.debug_bounds("sidebar-root").expect("the root row");
        let index = view.read_with(cx, |view, _| {
            let mut rows = Vec::new();
            flatten_work_folder_tree(
                view.work_folder.as_ref().unwrap().children(),
                1,
                &view.expanded_folders,
                &mut rows,
            );
            rows.iter()
                .position(|row| {
                    matches!(row.node, WorkFolderNode::Folder(folder) if folder.path() == directory)
                })
                .unwrap_or_else(|| panic!("{} is a visible row", directory.display()))
                + 1
        });
        let row_center = gpui::point(
            root_row.center().x,
            root_row.center().y + px(SIDEBAR_ROW_HEIGHT) * index as f32,
        );
        cx.simulate_mouse_move(row_center, None, gpui::Modifiers::none());
        cx.run_until_parked();
        let button = cx
            .debug_bounds("sidebar-folder-create-folder")
            .unwrap_or_else(|| panic!("{} renders its create button", directory.display()));
        assert!(button.top() <= row_center.y && row_center.y <= button.bottom());
    }
}
