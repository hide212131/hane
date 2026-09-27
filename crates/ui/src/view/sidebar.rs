use super::*;

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

    /// The folder/file tree for the sidebar, when this window was opened onto
    /// a work folder. File and folder rows reserve the same disclosure slot,
    /// so their file/folder icons line up and a folder does not shift when its
    /// disclosure icon changes direction.
    pub(super) fn work_folder_sidebar(
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
            );
        let mut filter_input = div().relative().flex_1().min_w(px(0.0)).h_full();
        if self.sidebar_filter.is_empty() {
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
        let query = self.sidebar_filter.to_lowercase();
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
                        div()
                            .id(("work-folder-entry", index))
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
                        div()
                            .id(("work-folder-folder", index))
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
                                } else if view.sidebar_filter.is_empty() {
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

/// The sidebar's file-row label: the file name unchanged, unless it embeds
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
