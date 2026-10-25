//! Sidebar pins (Issue #617): shortcuts to Markdown files and folders of the
//! current work folder, shown in their own area above the ordinary list.
//!
//! The ordered list lives in `hane_session`'s pin repository; this module
//! only holds the resolved copy for the current work folder, applies results
//! that belong to it (work-folder generation and root checked, newest
//! revision wins) and draws the area and the per-row toggle. Nothing here
//! reads the file system while rendering: whether a pin still resolves is
//! answered from the last successfully scanned tree.

use super::*;
use gpui_component::Icon;
use hane_session::{
    PinKind, PinSnapshot, PinUpdate, PinnedEntry, apply_pin_update, validate_pin_path,
};

/// Width and height of the per-row toggle slot, reserved on every row so the
/// button appearing never moves the name.
pub(super) const PIN_SLOT_SIZE: f32 = 24.0;
const PIN_ICON_SIZE: f32 = 14.0;
/// The hover group shared by a row and its toggle slot.
const PIN_ROW_GROUP: &str = "sidebar-pin-row";
/// The area (heading included) never exceeds this many rows.
const PIN_AREA_MAX_ROWS: f32 = 5.0;
/// ...nor this share of the room the list region has.
const PIN_AREA_MAX_FRACTION: f32 = 0.35;
/// The ordinary list keeps at least this many rows of room.
const PIN_AREA_MIN_LIST_ROWS: f32 = 2.0;

/// Which kind of request a result belongs to.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum PinRequest {
    Load,
    /// A pin/unpin the user asked for: shown only once it is saved.
    Explicit,
    /// A path change that already happened to the document tree.
    Followup,
}

/// Pin state of the current work folder.
pub(super) struct SidebarPins {
    pub(super) entries: Vec<PinnedEntry>,
    loaded: bool,
    /// An explicit toggle is being saved; further clicks are ignored.
    busy: bool,
    /// Why the stored list could not be used. While set, nothing is written.
    blocked: Option<String>,
    applied_revision: u64,
    /// Path changes applied to the display whose save failed. They are sent
    /// again ahead of the next update so an old path is never restored.
    unsaved: Vec<PinUpdate>,
    pub(super) scroll: ScrollHandle,
}

impl Default for SidebarPins {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
            loaded: false,
            busy: false,
            blocked: None,
            applied_revision: 0,
            unsaved: Vec::new(),
            scroll: ScrollHandle::new(),
        }
    }
}

impl SidebarPins {
    /// Forgets everything about the previous work folder.
    pub(super) fn reset(&mut self) {
        self.entries.clear();
        self.loaded = false;
        self.busy = false;
        self.blocked = None;
        self.applied_revision = 0;
        self.unsaved.clear();
        self.scroll.set_offset(point(px(0.0), px(0.0)));
    }
}

/// Where the pin area goes for a given list region.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct PinAreaLayout {
    /// Height of the separate, independently scrolling area; `0.0` when there
    /// is none.
    pub(super) height: f32,
    /// Rows (heading included) that are drawn at the top of the ordinary
    /// scrolling list instead, when the region is too low for a separate area.
    pub(super) inline_rows: usize,
}

/// Sizes the pin area: at most 35% of the list region and at most the heading
/// plus five rows, leaving the ordinary list two rows. When even the heading
/// and one row do not fit, the pins move into the ordinary list, so the whole
/// list scrolls and every pin stays reachable.
pub(super) fn pin_area_layout(pin_count: usize, list_region_height: f32) -> PinAreaLayout {
    if pin_count == 0 {
        return PinAreaLayout {
            height: 0.0,
            inline_rows: 0,
        };
    }
    let wanted = (1 + pin_count) as f32 * SIDEBAR_ROW_HEIGHT;
    let cap = (list_region_height * PIN_AREA_MAX_FRACTION)
        .min((1.0 + PIN_AREA_MAX_ROWS) * SIDEBAR_ROW_HEIGHT)
        .min(
            list_region_height
                - PIN_AREA_MIN_LIST_ROWS * SIDEBAR_ROW_HEIGHT
                - SIDEBAR_FILTER_GAP,
        );
    let height = wanted.min(cap);
    if height < 2.0 * SIDEBAR_ROW_HEIGHT {
        PinAreaLayout {
            height: 0.0,
            inline_rows: 1 + pin_count,
        }
    } else {
        PinAreaLayout {
            height,
            inline_rows: 0,
        }
    }
}

fn pin_display_name(entry: &PinnedEntry) -> String {
    let name = entry
        .relative_path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    if entry.kind == PinKind::File
        && let Some(stem) = name.strip_suffix(".md")
        && !stem.is_empty()
    {
        return stem.to_owned();
    }
    name
}

impl EditorView {
    /// `path` relative to the current work folder root, or `None` for the
    /// root itself and for anything outside it.
    pub(super) fn pin_relative(&self, path: &Path) -> Option<PathBuf> {
        let root = self.work_folder.as_ref()?.root();
        let relative = path.strip_prefix(root).ok()?;
        (!relative.as_os_str().is_empty()).then(|| relative.to_path_buf())
    }

    /// Whether a pin still resolves, answered from the retained tree only.
    fn pin_target_present(&self, absolute: &Path, kind: PinKind) -> bool {
        let Some(folder) = self.work_folder.as_ref() else {
            return false;
        };
        match kind {
            PinKind::File => folder.entry_for_path(absolute).is_some(),
            PinKind::Folder => {
                absolute != folder.root() && folder.children_at(absolute).is_some()
            }
        }
    }

    /// Input protections that also apply to pinning and pin navigation.
    fn pin_interaction_block(&self) -> Option<&'static str> {
        if self.inline_rename_active() {
            Some("名前を変更中のため、ピン留めは操作できません")
        } else if self.editor().ime().is_some() || self.sidebar_filter_has_composition() {
            Some("文字入力の変換中のため、ピン留めは操作できません")
        } else if self.tab_close_confirm.is_some() {
            Some("タブを閉じる確認中のため、ピン留めは操作できません")
        } else {
            None
        }
    }

    /// Starts loading the pins of the work folder that was just installed.
    pub(super) fn begin_pin_load(&mut self, cx: &mut Context<Self>) {
        let Some(root) = self
            .work_folder
            .as_ref()
            .map(|folder| folder.root().to_path_buf())
        else {
            return;
        };
        let generation = self.work_folder_generation;
        let repository = self.stores.pins();
        cx.spawn(async move |view, cx| {
            let task_root = root.clone();
            let result = cx
                .background_executor()
                .spawn(async move { repository.load(&task_root) })
                .await;
            let _ = view.update(cx, |view, cx| {
                view.finish_pin_request(
                    generation,
                    &root,
                    PinRequest::Load,
                    &[],
                    &[],
                    result,
                    cx,
                );
            });
        })
        .detach();
    }

    fn send_pin_updates(
        &mut self,
        updates: Vec<PinUpdate>,
        request: PinRequest,
        cx: &mut Context<Self>,
    ) {
        let Some(root) = self
            .work_folder
            .as_ref()
            .map(|folder| folder.root().to_path_buf())
        else {
            return;
        };
        let generation = self.work_folder_generation;
        let repository = self.stores.pins();
        // Path changes whose save failed earlier go first, so the stored list
        // never falls back to a path that no longer exists.
        let mut batch = self.sidebar_pins.unsaved.clone();
        batch.extend(updates.iter().cloned());
        if request == PinRequest::Explicit {
            self.sidebar_pins.busy = true;
        }
        let sent = batch.clone();
        cx.spawn(async move |view, cx| {
            let task_root = root.clone();
            let result = cx
                .background_executor()
                .spawn(async move { repository.update(&task_root, &batch) })
                .await;
            let _ = view.update(cx, |view, cx| {
                view.finish_pin_request(generation, &root, request, &sent, &updates, result, cx);
            });
        })
        .detach();
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "one result carries the request identity and its outcome"
    )]
    fn finish_pin_request(
        &mut self,
        generation: u64,
        root: &Path,
        request: PinRequest,
        sent: &[PinUpdate],
        updates: &[PinUpdate],
        result: Result<PinSnapshot, hane_session::PinError>,
        cx: &mut Context<Self>,
    ) {
        // A result for a work folder that has since been replaced belongs to
        // nobody: its state was reset with the switch.
        if generation != self.work_folder_generation
            || self
                .work_folder
                .as_ref()
                .is_none_or(|folder| folder.root() != root)
        {
            return;
        }
        if request == PinRequest::Explicit {
            self.sidebar_pins.busy = false;
        }
        match result {
            Ok(snapshot) => {
                self.sidebar_pins
                    .unsaved
                    .retain(|pending| !sent.contains(pending));
                if request == PinRequest::Load {
                    self.sidebar_pins.loaded = true;
                    self.sidebar_pins.blocked = None;
                }
                if snapshot.revision > self.sidebar_pins.applied_revision {
                    self.sidebar_pins.applied_revision = snapshot.revision;
                    let mut entries = snapshot.entries;
                    // Changes the display already follows but the store has
                    // not accepted yet must not be undone by this snapshot.
                    for pending in &self.sidebar_pins.unsaved {
                        let _ = apply_pin_update(&mut entries, pending);
                    }
                    self.sidebar_pins.entries = entries;
                }
            }
            Err(error) => {
                if request == PinRequest::Load {
                    self.sidebar_pins.loaded = true;
                    self.sidebar_pins.blocked = Some(error.to_string());
                } else if request == PinRequest::Followup {
                    self.sidebar_pins.unsaved.extend(updates.iter().cloned());
                }
                self.status = Some(error.to_string());
            }
        }
        cx.notify();
    }

    /// Pins or unpins `relative` with one click. The display changes only once
    /// the store has saved the change.
    pub(super) fn toggle_pin(&mut self, relative: PathBuf, kind: PinKind, cx: &mut Context<Self>) {
        if let Some(reason) = self.pin_interaction_block() {
            self.status = Some(reason.to_owned());
            cx.notify();
            return;
        }
        if let Some(reason) = self.sidebar_pins.blocked.clone() {
            self.status = Some(reason);
            cx.notify();
            return;
        }
        if !self.sidebar_pins.loaded {
            self.status = Some("ピン留めを読み込み中です".to_owned());
            cx.notify();
            return;
        }
        if self.sidebar_pins.busy {
            return;
        }
        let pinned = self
            .sidebar_pins
            .entries
            .iter()
            .any(|entry| entry.relative_path == relative);
        let update = if pinned {
            PinUpdate::Unpin(relative)
        } else {
            if let Err(error) = validate_pin_path(&relative) {
                self.status = Some(error.to_string());
                cx.notify();
                return;
            }
            PinUpdate::Pin(PinnedEntry::new(relative, kind))
        };
        self.send_pin_updates(vec![update], PinRequest::Explicit, cx);
        cx.notify();
    }

    /// Applies a path change that already happened in the document tree to
    /// the display at once and saves it. Nothing is written when no pin is
    /// affected.
    fn follow_pin_paths(&mut self, updates: Vec<PinUpdate>, cx: &mut Context<Self>) {
        let mut changed = false;
        for update in &updates {
            if let Ok(applied) = apply_pin_update(&mut self.sidebar_pins.entries, update) {
                changed |= applied;
            }
        }
        if self.sidebar_pins.loaded && !changed {
            return;
        }
        if self.sidebar_pins.blocked.is_some() {
            self.sidebar_pins.unsaved.extend(updates);
            self.status = Some(
                "ピン留めの保存データを扱えないため、変更後のパスを保存できませんでした".to_owned(),
            );
            cx.notify();
            return;
        }
        self.send_pin_updates(updates, PinRequest::Followup, cx);
    }

    /// A file or folder was renamed (or its H1 title moved it). The display
    /// follows the new path; the order is kept.
    pub(super) fn follow_pin_rename(
        &mut self,
        from: &Path,
        to: &Path,
        kind: PinKind,
        cx: &mut Context<Self>,
    ) {
        let (Some(from), Some(to)) = (self.pin_relative(from), self.pin_relative(to)) else {
            return;
        };
        let update = match kind {
            PinKind::File => PinUpdate::RenameFile { from, to },
            PinKind::Folder => PinUpdate::RenameFolder { from, to },
        };
        self.follow_pin_paths(vec![update], cx);
    }

    /// A file, or a folder with everything below it, was moved to the trash.
    pub(super) fn follow_pin_removal(&mut self, path: &Path, folder: bool, cx: &mut Context<Self>) {
        let Some(path) = self.pin_relative(path) else {
            return;
        };
        self.follow_pin_paths(vec![PinUpdate::Remove { path, folder }], cx);
    }

    /// A click on a pin row.
    pub(super) fn activate_pin(&mut self, relative: &Path, kind: PinKind, cx: &mut Context<Self>) {
        if let Some(reason) = self.pin_interaction_block() {
            self.status = Some(reason.to_owned());
            cx.notify();
            return;
        }
        let Some(root) = self
            .work_folder
            .as_ref()
            .map(|folder| folder.root().to_path_buf())
        else {
            return;
        };
        let absolute = root.join(relative);
        if !self.pin_target_present(&absolute, kind) {
            self.status = Some(format!(
                "ピン留め先が見つかりません: {}",
                relative.display()
            ));
            cx.notify();
            return;
        }
        match kind {
            PinKind::File => self.open_work_folder_entry(&absolute, cx),
            PinKind::Folder => self.reveal_work_folder_folder(&absolute, cx),
        }
    }

    /// Expands the ancestors of a folder (and the folder), selects it and
    /// brings its row into view. An active name filter is cleared first,
    /// because the row could otherwise be hidden by it. Unlike a click on the
    /// row in the ordinary tree this never collapses the folder.
    fn reveal_work_folder_folder(&mut self, path: &Path, cx: &mut Context<Self>) {
        if !self.cancel_inline_rename(cx) {
            return;
        }
        let Some(root) = self
            .work_folder
            .as_ref()
            .map(|folder| folder.root().to_path_buf())
        else {
            return;
        };
        self.blur_sidebar_filter(cx);
        if !self.sidebar_filter.text().is_empty() {
            self.sidebar_filter = ShortTextInput::default();
        }
        self.sidebar_keyboard_focus = true;
        let mut ancestor = path.parent();
        while let Some(parent) = ancestor {
            if parent == root || !parent.starts_with(&root) {
                break;
            }
            self.expanded_folders.insert(parent.to_path_buf());
            ancestor = parent.parent();
        }
        self.expanded_folders.insert(path.to_path_buf());
        self.selected_folder = Some(path.to_path_buf());
        self.sidebar_focus = SidebarFocus::Folder;
        self.scroll_sidebar_row_into_view(path);
        cx.notify();
    }

    /// Scrolls the ordinary list so the row of `path` is visible. Uses only
    /// the retained tree and the last measured viewport.
    fn scroll_sidebar_row_into_view(&self, path: &Path) {
        let Some(folder) = self.work_folder.as_ref() else {
            return;
        };
        let mut rows = Vec::new();
        flatten_work_folder_tree(folder.children(), 1, &self.expanded_folders, &mut rows);
        let Some(index) = rows.iter().position(|row| row.node.path() == path) else {
            return;
        };
        let draft_rows = self
            .work_folder_drafts
            .keys()
            .filter(|id| self.sessions.get(**id).is_some())
            .count();
        let content_height = sidebar_list_content_height(rows.len(), draft_rows, false);
        let viewport = f32::from(self.sidebar_scroll.bounds().size.height);
        if viewport <= 0.0 {
            return;
        }
        let max_scroll = (content_height - viewport).max(0.0);
        let row_top = (index + 1) as f32 * SIDEBAR_ROW_HEIGHT;
        let row_bottom = row_top + SIDEBAR_ROW_HEIGHT;
        let current = -f32::from(self.sidebar_scroll.offset().y);
        let target = if row_top < current {
            row_top
        } else if row_bottom > current + viewport {
            row_bottom - viewport
        } else {
            return;
        };
        self.sidebar_scroll
            .set_offset(point(px(0.0), px(-target.clamp(0.0, max_scroll))));
    }

    /// The toggle slot at the right end of a row. The slot always takes its
    /// space; only the button inside shows on row hover. Mouse-down on it is
    /// stopped here, so the row's open / expand / rename never sees the click.
    pub(super) fn pin_toggle_slot(
        &self,
        id: (&'static str, usize),
        relative: Option<PathBuf>,
        kind: PinKind,
        pinned: bool,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let label = if pinned {
            "ピン留めを解除"
        } else {
            "ピン留め"
        };
        let icon_path = if pinned {
            icons::ICON_PIN_OFF
        } else {
            icons::ICON_PIN
        };
        let view = cx.entity();
        let slot = div()
            .debug_selector(|| "sidebar-pin-toggle".to_owned())
            .w(px(PIN_SLOT_SIZE))
            .h(px(PIN_SLOT_SIZE))
            .flex_none()
            .invisible()
            .group_hover(PIN_ROW_GROUP, |style| style.visible())
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation());
        let Some(relative) = relative else {
            return slot;
        };
        slot.child(
            Button::new(id)
                .ghost()
                .w_full()
                .h_full()
                .tooltip(label)
                .accessibility_label(label)
                .icon(
                    Icon::empty()
                        .path(icon_path)
                        .size(px(PIN_ICON_SIZE))
                        .text_color(rgb(self.theme.sidebar_foreground)),
                )
                .on_click(move |_, _, app| {
                    let relative = relative.clone();
                    view.update(app, |view, cx| view.toggle_pin(relative, kind, cx));
                }),
        )
    }

    /// The heading and one row per pin, for the separate area or, when the
    /// region is too low, for the top of the ordinary list.
    pub(super) fn pin_area_rows(&self, cx: &mut Context<Self>) -> Vec<gpui::Stateful<gpui::Div>> {
        let Some(root) = self
            .work_folder
            .as_ref()
            .map(|folder| folder.root().to_path_buf())
        else {
            return Vec::new();
        };
        if self.sidebar_pins.entries.is_empty() {
            return Vec::new();
        }
        let active_path = self.sessions.active().path();
        let mut rows = vec![
            div()
                .id("sidebar-pin-heading")
                .h(px(SIDEBAR_ROW_HEIGHT))
                .flex_none()
                .px(px(SIDEBAR_ROW_HORIZONTAL_PADDING))
                .flex()
                .items_center()
                .text_color(rgb(self.theme.quote_foreground))
                .child("ピン留め"),
        ];
        for (index, entry) in self.sidebar_pins.entries.iter().enumerate() {
            let absolute = root.join(&entry.relative_path);
            let present = self.pin_target_present(&absolute, entry.kind);
            let is_active = present
                && entry.kind == PinKind::File
                && self.sidebar_focus == SidebarFocus::ActiveSession
                && active_path == Some(absolute.as_path());
            let is_selected = present
                && entry.kind == PinKind::Folder
                && self.sidebar_focus == SidebarFocus::Folder
                && self.selected_folder.as_deref() == Some(absolute.as_path());
            let icon_path = match entry.kind {
                PinKind::File => icons::ICON_FILE,
                PinKind::Folder => icons::ICON_FOLDER,
            };
            let color = if present {
                self.theme.sidebar_foreground
            } else {
                self.theme.quote_foreground
            };
            let parent = entry
                .relative_path
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
                .map(|parent| parent.to_string_lossy().into_owned());
            let mut name = pin_display_name(entry);
            if !present {
                name.push_str("（見つかりません）");
            }
            let relative = entry.relative_path.clone();
            let kind = entry.kind;
            let toggle_relative = entry.relative_path.clone();
            rows.push(
                div()
                    .id(("sidebar-pin-entry", index))
                    .debug_selector(|| "sidebar-pin-row".to_owned())
                    .group(PIN_ROW_GROUP)
                    .h(px(SIDEBAR_ROW_HEIGHT))
                    .flex_none()
                    .px(px(SIDEBAR_ROW_HORIZONTAL_PADDING))
                    .flex()
                    .flex_row()
                    .items_center()
                    .rounded_sm()
                    .cursor_pointer()
                    .when(is_active || is_selected, |element| {
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
                            .text_color(rgb(color))
                            .child(
                                gpui::svg()
                                    .path(icon_path)
                                    .flex_none()
                                    .size_4()
                                    .text_color(rgb(color)),
                            )
                            .child(div().flex_none().child(name))
                            .children(parent.map(|parent| {
                                div()
                                    .min_w(px(0.0))
                                    .text_color(rgb(self.theme.quote_foreground))
                                    .child(parent)
                            })),
                    )
                    .child(self.pin_toggle_slot(
                        ("sidebar-pin-toggle", index),
                        Some(toggle_relative),
                        kind,
                        true,
                        cx,
                    ))
                    .on_click(cx.listener(move |view, _, window, cx| {
                        window.focus(&view.focus_handle, cx);
                        view.activate_pin(&relative, kind, cx);
                    })),
            );
        }
        rows
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 0.001
    }

    #[test]
    fn no_pins_take_no_room() {
        let layout = pin_area_layout(0, 600.0);
        assert!(close(layout.height, 0.0));
        assert_eq!(layout.inline_rows, 0);
    }

    #[test]
    fn a_few_pins_use_their_own_height() {
        let layout = pin_area_layout(2, 600.0);
        assert!(close(layout.height, 3.0 * SIDEBAR_ROW_HEIGHT));
        assert_eq!(layout.inline_rows, 0);
    }

    #[test]
    fn many_pins_are_capped_by_rows_and_by_the_region_share() {
        let tall = pin_area_layout(40, 2000.0);
        assert!(close(
            tall.height,
            (1.0 + PIN_AREA_MAX_ROWS) * SIDEBAR_ROW_HEIGHT
        ));
        let short = pin_area_layout(40, 400.0);
        assert!(short.height <= 400.0 * PIN_AREA_MAX_FRACTION + 0.001);
        assert!(short.height >= 2.0 * SIDEBAR_ROW_HEIGHT);
        assert_eq!(short.inline_rows, 0);
    }

    #[test]
    fn a_very_low_region_moves_every_pin_into_the_scrolling_list() {
        let layout = pin_area_layout(8, 120.0);
        assert!(close(layout.height, 0.0));
        assert_eq!(layout.inline_rows, 9);
    }

    #[test]
    fn the_list_keeps_two_rows_whenever_the_area_is_separate() {
        for region in [150.0_f32, 200.0, 300.0, 500.0, 900.0] {
            let layout = pin_area_layout(50, region);
            if layout.height > 0.0 {
                assert!(
                    region - layout.height - SIDEBAR_FILTER_GAP
                        >= PIN_AREA_MIN_LIST_ROWS * SIDEBAR_ROW_HEIGHT - 0.001,
                    "region {region}"
                );
            }
        }
    }

    fn pin_test_root(label: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQUENCE: AtomicU64 = AtomicU64::new(0);
        std::env::temp_dir().join(format!(
            "hane-pins-{label}-{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ))
    }

    fn open_pin_view<'a>(
        cx: &'a mut gpui::TestAppContext,
        root: &Path,
        stores: StateStores,
    ) -> (gpui::Entity<EditorView>, &'a mut gpui::VisualTestContext) {
        cx.update(crate::actions::register_key_bindings);
        let work_folder = OsWorkFolderScanner.scan(root).unwrap();
        let (view, cx) = cx.add_window_view(move |_, cx| {
            EditorView::from_sessions(
                SessionSet::with_untitled("", "Untitled"),
                Arc::new(OsFileService),
                stores,
                cx,
            )
        });
        cx.simulate_resize(gpui::size(px(960.0), px(760.0)));
        view.update(cx, |view, cx| {
            view.work_folder = Some(work_folder);
            view.begin_pin_load(cx);
            cx.notify();
        });
        cx.run_until_parked();
        (view, cx)
    }

    fn view_root(view: &gpui::Entity<EditorView>, cx: &mut gpui::VisualTestContext) -> PathBuf {
        view.read_with(cx, |view, _| {
            view.work_folder
                .as_ref()
                .expect("a work folder is open")
                .root()
                .to_path_buf()
        })
    }

    fn pin_paths(entries: &[PinnedEntry]) -> Vec<PathBuf> {
        entries
            .iter()
            .map(|entry| entry.relative_path.clone())
            .collect()
    }

    #[gpui::test]
    fn clicking_the_toggle_pins_once_without_opening_the_row(cx: &mut gpui::TestAppContext) {
        let root = pin_test_root("toggle-click");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("Alpha.md"), "# Alpha\n").unwrap();
        let stores = StateStores::memory();
        let (view, cx) = open_pin_view(cx, &root, stores.clone());
        let root = view_root(&view, cx);

        let row = cx.debug_bounds("sidebar-file").expect("the file row");
        cx.simulate_mouse_move(row.center(), None, gpui::Modifiers::none());
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert!(
                view.sidebar_pins.entries.is_empty(),
                "hovering alone must not change the pins"
            );
        });

        let slot = cx.debug_bounds("sidebar-pin-toggle").expect("the toggle slot");
        assert!(
            slot.right() <= row.right() && slot.left() >= row.left(),
            "the toggle lives inside the row's reserved space"
        );
        cx.simulate_click(slot.center(), gpui::Modifiers::none());
        cx.run_until_parked();

        view.read_with(cx, |view, _| {
            assert_eq!(
                view.sidebar_pins.entries,
                vec![PinnedEntry::new("Alpha.md", PinKind::File)]
            );
            assert!(
                view.loading_paths.is_empty() && view.sessions.active().path().is_none(),
                "the click must not reach the row's open handler"
            );
        });
        assert_eq!(
            stores.pins().load(&root).unwrap().entries,
            vec![PinnedEntry::new("Alpha.md", PinKind::File)]
        );
        assert!(
            cx.debug_bounds("sidebar-pin-area").is_some(),
            "the pin area appears above the ordinary list"
        );

        std::fs::remove_dir_all(root).unwrap();
    }

    #[gpui::test]
    fn pin_order_is_newest_first_and_unpinning_keeps_the_rest(cx: &mut gpui::TestAppContext) {
        let root = pin_test_root("order");
        std::fs::create_dir_all(root.join("dir")).unwrap();
        std::fs::write(root.join("Alpha.md"), "# Alpha\n").unwrap();
        std::fs::write(root.join("dir/Beta.md"), "# Beta\n").unwrap();
        let stores = StateStores::memory();
        let (view, cx) = open_pin_view(cx, &root, stores.clone());
        let root = view_root(&view, cx);

        for (path, kind) in [
            ("Alpha.md", PinKind::File),
            ("dir", PinKind::Folder),
            ("dir/Beta.md", PinKind::File),
        ] {
            view.update(cx, |view, cx| view.toggle_pin(PathBuf::from(path), kind, cx));
            cx.run_until_parked();
        }
        view.read_with(cx, |view, _| {
            assert_eq!(
                pin_paths(&view.sidebar_pins.entries),
                [
                    PathBuf::from("dir/Beta.md"),
                    PathBuf::from("dir"),
                    PathBuf::from("Alpha.md")
                ]
            );
        });

        view.update(cx, |view, cx| {
            view.toggle_pin(PathBuf::from("dir"), PinKind::Folder, cx);
        });
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert_eq!(
                pin_paths(&view.sidebar_pins.entries),
                [PathBuf::from("dir/Beta.md"), PathBuf::from("Alpha.md")]
            );
        });
        assert_eq!(
            pin_paths(&stores.pins().load(&root).unwrap().entries),
            [PathBuf::from("dir/Beta.md"), PathBuf::from("Alpha.md")]
        );

        std::fs::remove_dir_all(root).unwrap();
    }

    #[gpui::test]
    fn a_result_for_a_replaced_work_folder_is_not_applied(cx: &mut gpui::TestAppContext) {
        let root = pin_test_root("stale");
        std::fs::create_dir_all(&root).unwrap();
        let (view, cx) = open_pin_view(cx, &root, StateStores::memory());
        let root = view_root(&view, cx);
        let snapshot = || PinSnapshot {
            entries: vec![PinnedEntry::new("Alpha.md", PinKind::File)],
            revision: u64::MAX,
        };

        view.update(cx, |view, cx| {
            let generation = view.work_folder_generation;
            // Another root's result for the current generation.
            view.finish_pin_request(
                generation,
                &root.join("other"),
                PinRequest::Load,
                &[],
                &[],
                Ok(snapshot()),
                cx,
            );
            assert!(view.sidebar_pins.entries.is_empty());
            // A result from before a work folder switch.
            view.work_folder_generation = generation.wrapping_add(1);
            view.finish_pin_request(
                generation,
                &root,
                PinRequest::Load,
                &[],
                &[],
                Ok(snapshot()),
                cx,
            );
            assert!(view.sidebar_pins.entries.is_empty());
            // The current one is applied.
            view.finish_pin_request(
                generation.wrapping_add(1),
                &root,
                PinRequest::Load,
                &[],
                &[],
                Ok(snapshot()),
                cx,
            );
            assert_eq!(view.sidebar_pins.entries.len(), 1);
        });

        std::fs::remove_dir_all(root).unwrap();
    }

    #[gpui::test]
    fn an_older_revision_does_not_replace_a_newer_list(cx: &mut gpui::TestAppContext) {
        let root = pin_test_root("revision");
        std::fs::create_dir_all(&root).unwrap();
        let (view, cx) = open_pin_view(cx, &root, StateStores::memory());
        let root = view_root(&view, cx);

        view.update(cx, |view, cx| {
            let generation = view.work_folder_generation;
            let newer = PinSnapshot {
                entries: vec![PinnedEntry::new("New.md", PinKind::File)],
                revision: u64::MAX - 1,
            };
            let older = PinSnapshot {
                entries: vec![PinnedEntry::new("Old.md", PinKind::File)],
                revision: u64::MAX - 2,
            };
            for snapshot in [newer, older] {
                view.finish_pin_request(
                    generation,
                    &root,
                    PinRequest::Followup,
                    &[],
                    &[],
                    Ok(snapshot),
                    cx,
                );
            }
            assert_eq!(
                pin_paths(&view.sidebar_pins.entries),
                [PathBuf::from("New.md")]
            );
        });

        std::fs::remove_dir_all(root).unwrap();
    }

    #[gpui::test]
    fn renames_and_trash_follow_the_pins_and_failures_keep_them(cx: &mut gpui::TestAppContext) {
        let root = pin_test_root("follow");
        std::fs::create_dir_all(root.join("dir")).unwrap();
        std::fs::write(root.join("Alpha.md"), "# Alpha\n").unwrap();
        std::fs::write(root.join("dir/Beta.md"), "# Beta\n").unwrap();
        let stores = StateStores::memory();
        let (view, cx) = open_pin_view(cx, &root, stores.clone());
        let root = view_root(&view, cx);
        for (path, kind) in [
            ("Alpha.md", PinKind::File),
            ("dir/Beta.md", PinKind::File),
            ("dir", PinKind::Folder),
        ] {
            view.update(cx, |view, cx| view.toggle_pin(PathBuf::from(path), kind, cx));
            cx.run_until_parked();
        }

        view.update(cx, |view, cx| {
            view.follow_pin_rename(
                &root.join("dir"),
                &root.join("notes"),
                PinKind::Folder,
                cx,
            );
        });
        cx.run_until_parked();
        let expected = [
            PathBuf::from("notes"),
            PathBuf::from("notes/Beta.md"),
            PathBuf::from("Alpha.md"),
        ];
        view.read_with(cx, |view, _| {
            assert_eq!(pin_paths(&view.sidebar_pins.entries), expected);
        });
        assert_eq!(
            pin_paths(&stores.pins().load(&root).unwrap().entries),
            expected
        );

        // A rename of something that is not pinned changes nothing.
        view.update(cx, |view, cx| {
            view.follow_pin_rename(
                &root.join("Other.md"),
                &root.join("Else.md"),
                PinKind::File,
                cx,
            );
        });
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert_eq!(pin_paths(&view.sidebar_pins.entries), expected);
        });

        view.update(cx, |view, cx| {
            view.follow_pin_removal(&root.join("notes"), true, cx);
        });
        cx.run_until_parked();
        let remaining = [PathBuf::from("Alpha.md")];
        view.read_with(cx, |view, _| {
            assert_eq!(pin_paths(&view.sidebar_pins.entries), remaining);
        });
        assert_eq!(
            pin_paths(&stores.pins().load(&root).unwrap().entries),
            remaining
        );

        std::fs::remove_dir_all(root).unwrap();
    }

    #[gpui::test]
    fn a_missing_pin_is_kept_but_never_opened(cx: &mut gpui::TestAppContext) {
        let root = pin_test_root("missing");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("Alpha.md"), "# Alpha\n").unwrap();
        let stores = StateStores::memory();
        let (view, cx) = open_pin_view(cx, &root, stores.clone());
        let root = view_root(&view, cx);
        stores
            .pins()
            .update(
                &root,
                &[PinUpdate::Pin(PinnedEntry::new("Gone.md", PinKind::File))],
            )
            .unwrap();
        view.update(cx, |view, cx| view.begin_pin_load(cx));
        cx.run_until_parked();

        view.update(cx, |view, cx| {
            assert_eq!(view.sidebar_pins.entries.len(), 1);
            view.activate_pin(Path::new("Gone.md"), PinKind::File, cx);
            assert!(view.loading_paths.is_empty());
            assert!(
                view.status
                    .as_deref()
                    .is_some_and(|status| status.contains("見つかりません"))
            );
            assert_eq!(
                view.sidebar_pins.entries.len(),
                1,
                "a missing pin stays until it is unpinned"
            );
        });

        std::fs::remove_dir_all(root).unwrap();
    }

    #[gpui::test]
    fn activating_a_folder_pin_expands_its_ancestors_and_selects_it(
        cx: &mut gpui::TestAppContext,
    ) {
        let root = pin_test_root("reveal");
        std::fs::create_dir_all(root.join("a/b")).unwrap();
        std::fs::write(root.join("a/b/Note.md"), "# Note\n").unwrap();
        let stores = StateStores::memory();
        let (view, cx) = open_pin_view(cx, &root, stores.clone());
        let root = view_root(&view, cx);
        view.update(cx, |view, cx| {
            view.toggle_pin(PathBuf::from("a/b"), PinKind::Folder, cx);
        });
        cx.run_until_parked();

        view.update(cx, |view, cx| {
            view.sidebar_filter = ShortTextInput::new("zzz".to_owned());
            view.activate_pin(Path::new("a/b"), PinKind::Folder, cx);
        });
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert!(view.expanded_folders.contains(&root.join("a")));
            assert!(view.expanded_folders.contains(&root.join("a/b")));
            assert_eq!(view.selected_folder, Some(root.join("a/b")));
            assert!(view.sidebar_filter.text().is_empty());
        });

        // Activating again never collapses it.
        view.update(cx, |view, cx| {
            view.activate_pin(Path::new("a/b"), PinKind::Folder, cx);
        });
        view.read_with(cx, |view, _| {
            assert!(view.expanded_folders.contains(&root.join("a/b")));
        });

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn display_names_drop_only_a_markdown_extension() {
        let file = PinnedEntry::new("設計/会議.md", PinKind::File);
        assert_eq!(pin_display_name(&file), "会議");
        let folder = PinnedEntry::new("設計.md", PinKind::Folder);
        assert_eq!(pin_display_name(&folder), "設計.md");
    }
}
