#![allow(
    clippy::single_match_else,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::float_cmp,
    clippy::redundant_closure_for_method_calls,
    clippy::too_many_lines,
    clippy::map_unwrap_or,
    clippy::doc_markdown,
    clippy::needless_pass_by_value,
    clippy::trivially_copy_pass_by_ref,
    clippy::semicolon_if_nothing_returned,
    clippy::unused_self,
    reason = "GPUI view callbacks and pixel geometry require these local framework-bound conventions"
)]

use crate::actions::install_action_listeners;
use crate::capture::InputCapture;
use crate::icons;
use crate::input::{InlineRenameInput, shape_inline_rename_line};
#[cfg(any(feature = "instrument", feature = "timing-probe"))]
use crate::instrument::{Instrumentation, log_summary};
#[cfg(test)]
use crate::line::DEFAULT_LINE_HEIGHT;
#[cfg(test)]
use crate::line::presented_block;
use crate::line::{
    BODY_FONT_SIZE, CARET_MODE_BADGE_HEIGHT, block_element, block_fits_sync_join_budget,
    expected_block_disclosures, presented_block_with_list_projection,
    presented_block_with_projections, row_element,
};
use crate::shape::WindowShaper;
use crate::theme::{DEFAULT_THEME, Theme, resolve_theme};
use gpui::{
    App, Bounds, ClickEvent, Context, CursorStyle, FocusHandle, Focusable, InteractiveElement,
    IntoElement, MagnifyEvent, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
    ParentElement, PathPromptOptions, Pixels, Render, ScrollDelta, ScrollHandle, ScrollWheelEvent,
    StatefulInteractiveElement, Styled, Subscription, Task, Window, div, point,
    prelude::FluentBuilder, px, rgb,
};
use hane_document::{
    Bias, BufferError, LineId, Revision, RevisionDelta, RopeBuffer, SourceOffset, SourceRange,
    TextBuffer,
};
use hane_editor::{Editor, EditorCommand, InputMeasurement, Selection};
use hane_markdown::{
    BlockId, BlockIndex, BlockIndexState, BlockIndexUpdate, IndexSource, IndexedBlock,
    ListProjection, local_block_index,
};
use hane_metrics::FrameMetrics;
#[cfg(test)]
use hane_presentation::{BlockKind, ListRowRole, StyleKind, VisualOffset};
use hane_presentation::{
    BlockLayout, HeightIndex, JoinedParse, LineShaper, ListCaretOrigin, ListEditingContext,
    MarkerEdge, VerticalMove, Visibility, VisualBlock, VisualLine, apply_list_editing_context,
    block_heights_with_disclosure, block_is_joinable, block_line_span, code_line_height,
    layout_block, parse_joined_span,
    trailing_blank_lines,
};
use hane_session::{
    CalendarDate, DateBadgeRange, DocumentSession, DraftId, DraftStore, FileEvent,
    FileEventOutcome, FileService, LoadedFile, OpenDecision, OpenPolicy, OsDraftStore,
    OsFileService, OsWorkFolderScanner, RecentFiles, RecoveredDrafts, SaveDecision, SaveFailure,
    SaveIntent, SaveOutcome, SaveTicket, SavedFile, SessionId, SessionSet, SessionViewState,
    Settings, StateStores, TitleSyncAction, WorkFolder, WorkFolderNode, WorkFolderScanner,
    date_badge_range, decide_title_sync, extract_h1_title, format_relative_date_label, local_today,
    run_save_job, split_file_name_for_badge, unique_folder_name, unique_markdown_filename,
};
use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use unicode_segmentation::UnicodeSegmentation;

const METRICS_CAPACITY: usize = 4_096;
/// Bound whole-block CPU work across all blocks and document switches in this
/// view. Do not queue missed viewports: a completion wakes the current viewport.
const MAX_JOINED_PARSE_JOBS: usize = 2;
const SIDEBAR_MIN_WIDTH: f32 = 160.0;
const SIDEBAR_MAX_WIDTH: f32 = 480.0;
const MAIN_MIN_WIDTH: f32 = 280.0;
const SIDEBAR_RESIZER_WIDTH: f32 = 5.0;
const SIDEBAR_ROW_HEIGHT: f32 = 24.0;
const SIDEBAR_TOOLBAR_HEIGHT: f32 = 28.0;
const SIDEBAR_TOOLBAR_GAP: f32 = 4.0;
const SIDEBAR_FILTER_HEIGHT: f32 = 28.0;
const SIDEBAR_FILTER_GAP: f32 = 4.0;
const SIDEBAR_PADDING: f32 = 8.0;
const SIDEBAR_ROW_HORIZONTAL_PADDING: f32 = 4.0;
/// How often `_date_badge_refresh_task` re-observes the local calendar date
/// for the sidebar's file-name badges. Short enough that a window left open
/// across local midnight shows `本日` moving on within about a minute of the
/// real boundary, long enough to stay well clear of "high-frequency polling":
/// each tick is a cheap local-time read, not a redraw unless the date
/// actually changed.
const DATE_BADGE_REFRESH_INTERVAL: Duration = Duration::from_secs(60);
/// Lower bound of the continuous zoom range (issue #228).
pub(crate) const MIN_ZOOM: f32 = 0.5;
/// Upper bound of the continuous zoom range (issue #228).
pub(crate) const MAX_ZOOM: f32 = 3.0;
/// Band around 100% zoom snaps to it. Narrow enough that a deliberate zoom
/// step lands past it on the very next step (Ctrl/Cmd+wheel steps by roughly
/// 8% per line), so the snap does not trap a continuing gesture at 100%.
const ZOOM_SNAP_RANGE: (f32, f32) = (0.98, 1.02);
/// Multiplicative zoom change per wheel "line" of Ctrl/Cmd+wheel input, i.e.
/// `zoom *= 2f32.powf(ZOOM_STEP_PER_LINE)` per line of scroll.
const ZOOM_STEP_PER_LINE: f32 = 0.08;
const SCROLLBAR_TRACK_WIDTH: f32 = 10.0;
const SCROLLBAR_THUMB_WIDTH: f32 = 6.0;
const SCROLLBAR_MIN_THUMB_HEIGHT: f32 = 28.0;
/// Sub-pixel tolerance for caret/badge visibility checks. Shaping and prefix
/// sums accumulate f32 rounding error; anything below half a device-independent
/// pixel is not a visible clip and must not keep a pending scroll request alive.
const CARET_VISIBILITY_TOLERANCE: f32 = 0.5;
/// Width of the sidebar's overlay scrollbar thumb while it is briefly shown
/// during a scroll. Kept well under half of `SCROLLBAR_THUMB_WIDTH` (the
/// editor's always-visible thumb) so the sidebar's idle right edge reads as
/// the thin `sidebar_resizer` boundary line rather than a second, thicker
/// scrollbar track (see issue #195).
const SIDEBAR_SCROLLBAR_THUMB_WIDTH: f32 = 3.0;
/// How long the sidebar's overlay scrollbar thumb stays shown after the most
/// recent wheel/trackpad scroll or thumb drag before it fades back out.
const SIDEBAR_SCROLLBAR_HIDE_DELAY: Duration = Duration::from_millis(600);

#[derive(Clone, Copy, Debug)]
struct SidebarResizeDrag {
    pointer_x: f32,
    sidebar_width: f32,
}

#[derive(Clone, Copy, Debug)]
struct ScrollbarDrag {
    pointer_y: f32,
    scroll_y: f32,
    viewport_height: f32,
    content_height: f32,
}

/// Which edge of the editor viewport a text-selection drag is currently held
/// against, and therefore which way `step_text_autoscroll` extends the
/// selection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AutoscrollDirection {
    Up,
    Down,
}

/// How close to the editor viewport's top or bottom edge a text-selection
/// drag's pointer has to be to start autoscrolling (issue #213).
const TEXT_SELECTION_AUTOSCROLL_EDGE: f32 = 24.0;
/// How often a held text-selection drag extends the selection by one more
/// row and lets `scroll_cursor_into_view` follow it while the pointer stays
/// inside `TEXT_SELECTION_AUTOSCROLL_EDGE`, without needing to move.
const TEXT_SELECTION_AUTOSCROLL_INTERVAL: Duration = Duration::from_millis(40);

/// Blocks kept presented on each side of the viewport, so scrolling back a
/// screen does not re-present what was just drawn.
const BLOCK_CACHE_MARGIN: usize = 64;

fn scroll_y_for_cursor(
    scroll_y: f32,
    cursor_top: f32,
    line_height: f32,
    viewport_height: f32,
) -> f32 {
    if cursor_top < scroll_y {
        cursor_top
    } else if cursor_top + line_height > scroll_y + viewport_height {
        cursor_top + line_height - viewport_height
    } else {
        scroll_y
    }
}

fn content_top_for_scroll(scroll_y: f32) -> f32 {
    -scroll_y
}

fn clamp_scroll_y(scroll_y: f32, content_height: f32, viewport_height: f32) -> f32 {
    let max_scroll = (content_height - viewport_height).max(0.0);
    scroll_y.clamp(0.0, max_scroll)
}

fn scrollbar_thumb_geometry(
    viewport_height: f32,
    content_height: f32,
    scroll_y: f32,
) -> Option<(f32, f32)> {
    if viewport_height <= 0.0 || content_height <= viewport_height {
        return None;
    }
    let max_scroll = content_height - viewport_height;
    let thumb_height = (viewport_height * viewport_height / content_height)
        .max(SCROLLBAR_MIN_THUMB_HEIGHT)
        .min(viewport_height);
    let travel = (viewport_height - thumb_height).max(0.0);
    let top = if travel == 0.0 {
        0.0
    } else {
        clamp_scroll_y(scroll_y, content_height, viewport_height) / max_scroll * travel
    };
    Some((top, thumb_height))
}

fn scroll_y_for_thumb_drag(
    start_scroll_y: f32,
    pointer_delta: f32,
    viewport_height: f32,
    content_height: f32,
) -> f32 {
    let Some((_, thumb_height)) =
        scrollbar_thumb_geometry(viewport_height, content_height, start_scroll_y)
    else {
        return 0.0;
    };
    let max_scroll = (content_height - viewport_height).max(0.0);
    let travel = (viewport_height - thumb_height).max(0.0);
    if travel == 0.0 {
        0.0
    } else {
        (start_scroll_y + pointer_delta * max_scroll / travel).clamp(0.0, max_scroll)
    }
}

fn sidebar_width_for_drag(start_width: f32, pointer_delta: f32, viewport_width: f32) -> f32 {
    let available = (viewport_width - MAIN_MIN_WIDTH - SIDEBAR_RESIZER_WIDTH).max(80.0);
    let minimum = SIDEBAR_MIN_WIDTH.min(available);
    let maximum = SIDEBAR_MAX_WIDTH.min(available).max(minimum);
    (start_width + pointer_delta).clamp(minimum, maximum)
}

fn sidebar_list_content_height(
    tree_rows: usize,
    draft_rows: usize,
    empty_filter_row: bool,
) -> f32 {
    (1 + tree_rows + draft_rows + usize::from(empty_filter_row)) as f32 * SIDEBAR_ROW_HEIGHT
}

/// The width layout wraps against: the window viewport, minus whatever the
/// sidebar takes and the padding on both sides of a line. Row painting and
/// `layout_block` must agree on this number, or a row could measure narrower
/// than what is actually drawn and clip.
fn text_column_width(viewport_width: f32, sidebar_width: f32, padding: f32) -> f32 {
    (viewport_width - sidebar_width - 2.0 * padding).max(1.0)
}

/// Clamps a zoom level to Hane's supported 50%-300% range and snaps it to
/// exactly 100% inside `ZOOM_SNAP_RANGE`. Snapping to the exact value (rather
/// than just narrowing the range) means the next step away from 100% is a
/// full `ZOOM_STEP_PER_LINE`-sized step outside the band, so a continuing
/// gesture always breaks free instead of hovering near 100% indefinitely.
fn clamp_and_snap_zoom(zoom: f32) -> f32 {
    let clamped = zoom.clamp(MIN_ZOOM, MAX_ZOOM);
    let (low, high) = ZOOM_SNAP_RANGE;
    if (low..=high).contains(&clamped) {
        1.0
    } else {
        clamped
    }
}

/// Multiplicative zoom change for one Ctrl/Cmd+wheel event, matching the
/// convention that scrolling "up" (the direction that also moves
/// `scroll_y` toward the document start) zooms in.
fn zoom_factor_for_wheel(delta: ScrollDelta, line_height: f32) -> f32 {
    let lines = match delta {
        ScrollDelta::Lines(delta) => delta.y,
        ScrollDelta::Pixels(delta) => f32::from(delta.y) / line_height.max(1.0),
    };
    2f32.powf(lines * ZOOM_STEP_PER_LINE)
}

fn height_snapshot_matches_line_height(current: f32, snapshot: f32) -> bool {
    current.to_bits() == snapshot.to_bits()
}

fn block_context_revision_is_current(current: Revision, candidate: Revision) -> bool {
    current == candidate
}

/// Captured when a zoom-changing gesture (Ctrl/Cmd+wheel, trackpad pinch, or
/// the Ctrl/Cmd+0 reset) fires, so that once the new zoom's heights are
/// installed later in the same frame, `render` can put the same document
/// position back at the same window position instead of letting the resize
/// of every item above it shift what is on screen.
#[derive(Clone, Copy, Debug)]
struct PendingZoomAnchor {
    /// Item ordinal (block, or physical line before a `BlockIndex` exists) in
    /// `self.heights` the gesture's position fell on.
    ordinal: usize,
    /// Fraction (0.0-1.0) of the way down that item's height the gesture's
    /// position fell, so the anchor tracks a point inside the item and not
    /// just its top edge.
    fraction: f32,
    /// The gesture's position, in content-local window coordinates (window
    /// y minus the header height), that `fraction` should keep resolving to.
    window_offset: f32,
}

/// Identifies the document a background job was started for. A result that
/// comes back for another session, or for a document that has since been
/// replaced, is dropped instead of published.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct DocumentKey {
    session: SessionId,
    generation: u64,
}

/// A not-yet-named work-folder note's recovery-journal id and the folder it
/// will be created in once its first H1 lands. Recorded per draft, rather
/// than assumed to be the work folder root, so "new note" in a selected
/// subfolder actually creates the file there.
#[derive(Clone, Debug, Eq, PartialEq)]
struct WorkFolderDraft {
    draft_id: DraftId,
    target_directory: PathBuf,
}

/// One H1-driven rename attempt's outcome-handling context, bundled so
/// `finish_title_rename` takes one argument instead of five.
struct TitleRenameAttempt {
    id: SessionId,
    ticket: SaveTicket,
    from: PathBuf,
    target: PathBuf,
    title: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum InlineRenameKind {
    File,
    Folder,
}

/// State for the one sidebar row currently being edited. The editable text is
/// kept separate from the fixed Markdown extension so input and filesystem
/// validation cannot accidentally turn a note into another file type.
struct InlineRename {
    kind: InlineRenameKind,
    from: PathBuf,
    text: String,
    fixed_extension: Option<String>,
    selected_range: Range<usize>,
    selection_reversed: bool,
    marked_range: Option<Range<usize>>,
    composition: Option<InlineRenameComposition>,
    pending: bool,
}

#[derive(Clone, Debug)]
struct InlineRenameComposition {
    text: String,
    selected_range: Range<usize>,
    selection_reversed: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct InlineRenameRenderState {
    pub(crate) text: String,
    pub(crate) selected_range: Range<usize>,
    pub(crate) selection_reversed: bool,
    pub(crate) marked_range: Option<Range<usize>>,
}

#[derive(Clone, Debug)]
struct SidebarFilterComposition {
    text: String,
    selected_range: Range<usize>,
    selection_reversed: bool,
}

fn inline_rename_parts(path: &Path, kind: InlineRenameKind) -> Option<(String, Option<String>)> {
    // The inline field is UTF-8 text. Refuse names that cannot be represented
    // losslessly instead of letting `to_string_lossy` silently replace bytes
    // and turning an unchanged Enter into a destructive rename.
    let name = path.file_name()?.to_str()?.to_owned();
    if kind == InlineRenameKind::File
        && let Some(extension) = path.extension().and_then(|extension| extension.to_str())
        && extension.eq_ignore_ascii_case("md")
    {
        let fixed_extension = format!(".{extension}");
        let stem = name
            .strip_suffix(&fixed_extension)
            .unwrap_or(&name)
            .to_owned();
        return Some((stem, Some(fixed_extension)));
    }
    Some((name, None))
}

fn valid_inline_rename_name(name: &str) -> bool {
    !name.is_empty() && name != "." && name != ".." && !name.contains('/') && !name.contains('\\')
}

fn rebase_ui_path(path: &Path, from: &Path, to: &Path) -> Option<PathBuf> {
    let relative = path.strip_prefix(from).ok()?;
    Some(if relative.as_os_str().is_empty() {
        to.to_path_buf()
    } else {
        to.join(relative)
    })
}

fn byte_offset_from_utf16(text: &str, offset: usize) -> usize {
    let mut utf16 = 0;
    for (byte, character) in text.char_indices() {
        if utf16 >= offset {
            return byte;
        }
        utf16 += character.len_utf16();
    }
    text.len()
}

fn utf16_offset_from_byte(text: &str, offset: usize) -> usize {
    let mut utf16 = 0;
    for (byte, character) in text.char_indices() {
        if byte >= offset {
            break;
        }
        utf16 += character.len_utf16();
    }
    utf16
}

fn byte_range_from_utf16(text: &str, range: &Range<usize>) -> Range<usize> {
    byte_offset_from_utf16(text, range.start)..byte_offset_from_utf16(text, range.end)
}

fn inline_rename_selected_range(
    replacement_start: usize,
    replacement: &str,
    selected_range_utf16: Option<Range<usize>>,
    fallback: Range<usize>,
) -> Range<usize> {
    selected_range_utf16
        .map(|selected| {
            let selected_start = byte_offset_from_utf16(replacement, selected.start);
            let selected_end = byte_offset_from_utf16(replacement, selected.end);
            (replacement_start + selected_start)..(replacement_start + selected_end)
        })
        .unwrap_or(fallback)
}

fn range_to_utf16(text: &str, range: &Range<usize>) -> Range<usize> {
    utf16_offset_from_byte(text, range.start)..utf16_offset_from_byte(text, range.end)
}

fn previous_inline_rename_boundary(text: &str, offset: usize) -> usize {
    text[..offset]
        .grapheme_indices(true)
        .next_back()
        .map_or(0, |(index, _)| index)
}

fn next_inline_rename_boundary(text: &str, offset: usize) -> usize {
    text[offset..]
        .grapheme_indices(true)
        .nth(1)
        .map_or(text.len(), |(index, _)| offset + index)
}

fn inline_rename_cursor(rename: &InlineRename) -> usize {
    if rename.selection_reversed {
        rename.selected_range.start
    } else {
        rename.selected_range.end
    }
}

fn select_inline_rename_to(rename: &mut InlineRename, offset: usize) {
    select_inline_rename_to_fields(
        &mut rename.selected_range,
        &mut rename.selection_reversed,
        offset,
    );
}

fn select_inline_rename_to_fields(
    selected_range: &mut Range<usize>,
    selection_reversed: &mut bool,
    offset: usize,
) {
    if *selection_reversed {
        selected_range.start = offset;
    } else {
        selected_range.end = offset;
    }
    if selected_range.end < selected_range.start {
        *selection_reversed = !*selection_reversed;
        *selected_range = selected_range.end..selected_range.start;
    }
}

/// Which of the sidebar's two selection sources is currently shown
/// highlighted. The two are tracked separately because they change on
/// different events — the active session swaps whenever a file or draft is
/// opened, while `selected_folder` only changes on an explicit folder/root
/// click — so without this flag both could end up highlighted at once (see
/// issue #47).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
enum SidebarFocus {
    /// A file or draft row is highlighted, driven by the active session.
    #[default]
    ActiveSession,
    /// A folder or root row is highlighted, driven by `selected_folder`.
    Folder,
}

pub struct EditorView {
    /// Every open document. Holding a set rather than one editor is what lets a
    /// filer add and switch documents without the renderer changing.
    sessions: SessionSet,
    /// The only route to the filesystem. Reads and writes are handed to it on a
    /// background thread, so no file operation sits on the input path.
    files: Arc<dyn FileService>,
    stores: StateStores,
    settings: Settings,
    recent: RecentFiles,
    /// The Markdown index of the directory this window was opened onto, if
    /// any. `None` keeps single-file editing exactly as it was: no sidebar,
    /// no folder concept anywhere else in the view.
    work_folder: Option<WorkFolder>,
    /// Where a not-yet-named work-folder note's content is journalled, so a
    /// crash before it earns a real filename never loses it. Removed once the
    /// session it belongs to gets a real path or closes.
    draft_store: Arc<dyn DraftStore>,
    work_folder_drafts: HashMap<SessionId, WorkFolderDraft>,
    /// The folder the sidebar currently has selected, used as the target
    /// directory for the next "new note" or "new folder". `None` means the
    /// work folder root. This is independent of what the sidebar shows
    /// highlighted (see `sidebar_focus`): opening a file leaves this
    /// unchanged, so "select a folder, then create a note" keeps working
    /// even though the folder row itself stops being highlighted.
    selected_folder: Option<PathBuf>,
    /// Which selection source (`selected_folder` or the active session) the
    /// sidebar currently highlights. Exclusive by construction, so a file and
    /// a folder row can never both show as selected at once.
    sidebar_focus: SidebarFocus,
    /// Folders the sidebar tree currently shows expanded. The work folder
    /// root itself is always shown expanded and is not tracked here.
    expanded_folders: HashSet<PathBuf>,
    /// The current file-name-only sidebar filter. It is deliberately kept as
    /// view state rather than in `WorkFolder`, because filtering is a display
    /// concern and must not change the discovered tree.
    sidebar_filter: String,
    sidebar_filter_selected_range: Range<usize>,
    sidebar_filter_selection_reversed: bool,
    sidebar_filter_marked_range: Option<Range<usize>>,
    sidebar_filter_composition: Option<SidebarFilterComposition>,
    /// The shared view focus handle is used by both the editor and sidebar
    /// inputs, so this bit identifies which text field currently owns it.
    sidebar_filter_focused: bool,
    sidebar_filter_input_bounds: Option<Bounds<Pixels>>,
    /// Whether the most recent keyboard focus came from the sidebar. The
    /// editor and sidebar share one view focus handle, so this explicit bit
    /// prevents an F2 pressed while editing document text from renaming the
    /// active file merely because its row is highlighted.
    sidebar_keyboard_focus: bool,
    /// The row-local inline rename field, if one is active.
    inline_rename: Option<InlineRename>,
    /// User-adjustable width of the work-folder sidebar.
    sidebar_width: f32,
    /// Active drag of the vertical divider between sidebar and editor.
    sidebar_resize_drag: Option<SidebarResizeDrag>,
    /// Native scroll state for the work-folder list; a custom thumb mirrors it.
    sidebar_scroll: ScrollHandle,
    /// Active drag of the sidebar's visible scrollbar thumb.
    sidebar_scrollbar_drag: Option<ScrollbarDrag>,
    /// Active drag of the editor's visible scrollbar thumb.
    editor_scrollbar_drag: Option<ScrollbarDrag>,
    /// Set while a left-button drag started by `on_row_mouse_down` is still
    /// held, so `on_panel_mouse_move` can tell a text-selection drag apart
    /// from a sidebar-resize or scrollbar drag once the pointer leaves every
    /// row (e.g. above the top row or below the bottom one) and keep
    /// autoscrolling it (issue #213).
    text_selection_drag: bool,
    /// Which edge of the editor viewport the current text-selection drag is
    /// held against, if any. `None` means no autoscroll is currently ticking.
    text_autoscroll: Option<AutoscrollDirection>,
    /// Bumped every time `text_autoscroll` changes or the document is
    /// replaced, so a running autoscroll timer loop can tell it has been
    /// superseded (pointer left the edge zone, the drag ended, reversed
    /// direction, or the drag's document disappeared) and stop rescheduling
    /// itself instead of ticking a drag it no longer belongs to.
    text_autoscroll_activity: u64,
    /// Whether the sidebar's overlay scrollbar thumb is currently shown. Set
    /// on a wheel/trackpad scroll or thumb drag and cleared by the delayed
    /// hide task armed in `show_sidebar_scrollbar_briefly` once
    /// `sidebar_scrollbar_activity` shows no newer scroll happened meanwhile.
    sidebar_scrollbar_visible: bool,
    /// Generation counter bumped on every call to
    /// `show_sidebar_scrollbar_briefly`, so a hide task armed by an earlier
    /// scroll can tell it has been superseded by a later one and skip hiding.
    sidebar_scrollbar_activity: u64,
    /// The local calendar date the sidebar's file-name badges last used for
    /// "today" (see `refresh_sidebar_date_badge_today`). The render path
    /// itself always reads a fresh `local_today()`; this is only kept so the
    /// periodic refresh can tell a real date-boundary crossing apart from a
    /// same-day recheck and skip the `cx.notify()` when nothing changed.
    sidebar_date_badge_today: CalendarDate,
    /// Keeps `refresh_sidebar_date_badge_today`'s periodic recheck alive for
    /// the life of the view; dropping it (which happens when the view
    /// itself drops) cancels the task, so no polling outlives the view.
    _date_badge_refresh_task: Task<()>,
    /// Folder paths reserved by a `new_work_folder_folder` call whose
    /// `create_dir` is still in flight. `work_folder`'s tree is not updated
    /// until that background write completes, so a name picked from the
    /// tree alone would let a second "new folder" click before the first
    /// finishes reserve the same name; this set is checked alongside the
    /// tree so the second click picks the next name instead.
    pending_new_folders: HashSet<PathBuf>,
    /// The last laid-out bounds of the inline rename input. Native text-input
    /// callbacks report window coordinates rather than the element hitbox, so
    /// this lets click-to-caret conversion use the same geometry the input
    /// just painted.
    inline_rename_input_bounds: Option<Bounds<Pixels>>,
    /// Keeps the app-quit draft flush (see `flush_pending_drafts`) alive for
    /// the life of the view; dropping it would cancel the hook.
    _quit_subscription: Subscription,
    /// The active keyboard input mode, when the platform can determine it.
    /// Drives the caret's small input-mode badge; refreshed by
    /// `_input_mode_subscription` so it updates without polling.
    caret_input_mode: Option<gpui::KeyboardInputMode>,
    /// Keeps the input-source change hook (see `caret_input_mode`)
    /// alive for the life of the view; dropping it would cancel the hook.
    _input_mode_subscription: Subscription,
    /// Refreshes the input mode when the editor receives focus, covering a
    /// pre-existing IME state before any mode-change notification arrives.
    _input_mode_focus_subscription: Option<Subscription>,
    /// A draft-recovery failure from the last work-folder scan, if any. Kept
    /// apart from `status`: opening the work folder's first note runs right
    /// after the scan and drives `status` through "Opening…" and "Opened" in
    /// the same update cycle, which would otherwise blank out the warning
    /// before the user ever saw it. Cleared only by the next scan's outcome.
    draft_recovery_warning: Option<String>,
    /// The title an in-flight create-or-rename write is for, so `finish_save`
    /// can mark the session auto-managed once the write actually lands
    /// rather than when it was merely requested.
    title_sync_pending: HashMap<SessionId, String>,
    /// Sessions with an H1-driven filename probe or rename in flight, so a
    /// second debounce firing before the first completes is skipped instead
    /// of racing it into a duplicate file.
    title_sync_in_flight: HashSet<SessionId>,
    /// The newest H1 debounce still waiting to fire for each session. A folder
    /// rename must wait for this too, because an unnamed draft's timer carries
    /// its target directory into the background create operation.
    title_sync_scheduled: HashMap<SessionId, Revision>,
    /// H1 sync timers that fired while a filesystem rename was already pending.
    /// They are retried after the user-controlled rename releases the path.
    title_sync_deferred: HashSet<SessionId>,
    /// Paths with a background read in flight, so a second click on a note
    /// that has not finished loading yet does not start a second read.
    loading_paths: HashSet<PathBuf>,
    /// The path most recently asked to be opened, whether or not it has
    /// finished loading yet. A load that lands after a newer request must
    /// not switch what is on screen away from that newer request.
    latest_open_target: Option<PathBuf>,
    /// Bumped every time `switch_to_work_folder` replaces `sessions` and
    /// `work_folder` wholesale. A background read started against the
    /// previous folder (or single-file state) carries the generation it was
    /// requested under; `finish_open` discards a result whose generation no
    /// longer matches instead of merging a stale file into whatever folder
    /// is open now, since `into`/`latest_open_target` alone cannot tell a
    /// same-named stale request in the new folder apart from one in the old.
    work_folder_generation: u64,
    pub(crate) focus_handle: FocusHandle,
    heights: HeightIndex,
    scroll_y: f32,
    viewport_height: f32,
    pub(crate) metrics: FrameMetrics,
    status: Option<String>,
    theme: Theme,
    #[cfg(any(feature = "instrument", feature = "timing-probe"))]
    pub(crate) instrumentation: Instrumentation,
    background_presentation_generation: u64,
    /// Presented blocks, keyed by the index's stable block id so an entry
    /// survives typing elsewhere in the document.
    block_cache: HashMap<BlockId, VisualBlock>,
    /// Physical line to the block that drew it, rebuilt each frame. Mouse hit
    /// testing addresses lines, so it needs the reverse of the render mapping.
    line_owners: HashMap<usize, (BlockId, usize)>,
    /// Rows for each presented block, keyed like `block_cache`. Kept across
    /// frames so scrolling back, or typing in another block, does not re-shape
    /// text that has not changed.
    layout_cache: HashMap<BlockId, LayoutCacheEntry>,
    /// Width of the text column the rows were laid out for. A change to it
    /// invalidates every layout, which is why it is recorded rather than
    /// recomputed.
    content_width: f32,
    /// Horizontal offset of the main column from the window's left edge,
    /// i.e. the sidebar and its resizer when a work folder is open, zero
    /// otherwise. Row mouse events report window-space coordinates, so this
    /// has to be subtracted before it is used to hit-test text.
    main_column_left: f32,
    /// Hash of the window font properties `WindowShaper` used, including the
    /// zoom level folded into every font size (see
    /// `WindowShaper::font_revision`). Width and document revision live in
    /// each cache entry; this is the remaining global invalidation generation.
    layout_font_revision: u64,
    /// Document zoom level; 1.0 is 100%. Scales body font size, row height
    /// and soft-wrap width (issue #228). Clamped to `MIN_ZOOM..=MAX_ZOOM` and
    /// snapped to exactly 1.0 near it by `clamp_and_snap_zoom`.
    zoom: f32,
    /// The unsnapped zoom level accumulated from the current gesture stream.
    /// `zoom` is the effective display value and may stay at 1.0 while this
    /// crosses the snap band; keeping the raw value prevents small deltas from
    /// being discarded one event at a time.
    raw_zoom: f32,
    /// Set by a zoom-changing gesture, consumed after the visible blocks have
    /// been remeasured for the new zoom. See `PendingZoomAnchor`.
    pending_zoom_anchor: Option<PendingZoomAnchor>,
    /// Where the caret was drawn last frame, relative to the content area. The
    /// IME asks for this to place its candidate window.
    caret_geometry: Option<CaretGeometry>,
    /// Set after an editor command changes the caret/selection. The immediate
    /// scroll uses the previous frame's layout; the request remains armed until
    /// a post-layout pass observes stable geometry when progressive disclosure
    /// changes a row height (for example, an inactive zero-height code fence
    /// becoming editable).
    pending_caret_visibility_after_layout: bool,
    /// Semantic list owner retained for the empty line created by the most
    /// recent list-item newline. It is presentation-only and is cleared by
    /// the next input, movement or selection operation.
    pending_list_editing: Option<ListEditingContext>,
    /// Markdown block boundaries for the current revision. Updated incrementally
    /// on the input path and republished by the background parse; the publish
    /// priority between the two lives in `BlockIndexState`.
    block_index: BlockIndexState,
    /// What one entry of `heights` measures. Blocks as soon as an index is
    /// published, physical lines until then.
    granularity: Granularity,
    /// Identity and cheap height input aligned with a block-granularity height
    /// index. This lets an incremental BlockIndex update splice only its changed
    /// run while retaining measured heights on both sides.
    height_blocks: HeightBlocks,
    document_parse_job_running: bool,
    /// The disclosure for which the last background height snapshot was
    /// requested. Selection drags can change this on every pointer event;
    /// keeping it here coalesces identical requests without walking the whole
    /// selected block range on the input thread. Empty disclosures matter too:
    /// they are the snapshot that collapses the middle of a selection after
    /// the selection is reduced to a caret.
    last_background_height_disclosure: Option<(Revision, SourceRange)>,
    /// The disclosure represented by the current height tree at the last
    /// bounded synchronization. Keeping the previous caret endpoints lets a
    /// move away from a zero-height fence shrink that old block without
    /// scanning the intervening document.
    last_applied_height_disclosure: Option<(Revision, SourceRange)>,
    /// Whole-span parse of a joinable block too large for `presented_block` to
    /// read and reparse synchronously on every viewport miss (see
    /// `block_fits_sync_join_budget`), keyed like `block_cache`. Populated by
    /// `schedule_joined_parse`, off the render path, and is what lets such a
    /// block still resolve a marker pair arbitrarily far apart without this
    /// view reading the whole block on every scroll.
    joined_parse_cache: HashMap<BlockId, JoinedBlockCache>,
    /// Blocks with a `schedule_joined_parse` background job in flight, so a
    /// block already being parsed is not queued again on the next frame.
    joined_parse_jobs: HashMap<BlockId, JoinedParseJob>,
    /// Includes detached jobs from previous documents until their completion.
    /// Unlike the per-document deduplication map, replacement must not reset it.
    joined_parse_jobs_running: usize,
}

/// Identity of a background request. A late completion must not clear the
/// in-flight entry of another document or a newer snapshot of the same block.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct JoinedParseJob {
    document: DocumentKey,
    revision: Revision,
    source_range: SourceRange,
}

/// One joinable block's cached whole-span parse (see
/// `EditorView::joined_parse_cache`), and the block identity it was computed
/// for. An entry whose `revision` or `source_range` no longer matches the
/// index's current answer for the block is stale and is not used.
struct JoinedBlockCache {
    revision: Revision,
    source_range: SourceRange,
    parse: JoinedParse,
}

/// The caret rectangle, in coordinates relative to the top-left of the content
/// area, as the last frame drew it.
#[derive(Clone, Copy, Debug)]
pub(crate) struct CaretGeometry {
    pub x: f32,
    pub y: f32,
    pub height: f32,
}

/// The unit `heights` is keyed by.
///
/// Block granularity is the R4A target and the steady state. Line granularity is
/// the startup path: a document-wide index costs a full parse, so the first
/// frames after a document is opened are laid out per line, with block kinds for
/// the viewport coming from a bounded local parse. The renderer itself always
/// draws whole blocks; only what a height entry measures differs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Granularity {
    Blocks,
    Lines,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct HeightBlock {
    id: BlockId,
    line_count: usize,
}

const HEIGHT_BLOCK_CHUNK_TARGET: usize = 128;

#[derive(Clone, Debug, Default)]
struct HeightBlocks {
    chunks: Vec<Vec<HeightBlock>>,
    counts: Vec<usize>,
    len: usize,
}

impl FromIterator<HeightBlock> for HeightBlocks {
    fn from_iter<T: IntoIterator<Item = HeightBlock>>(iter: T) -> Self {
        let blocks = iter.into_iter().collect::<Vec<_>>();
        let chunks = blocks
            .chunks(HEIGHT_BLOCK_CHUNK_TARGET)
            .map(<[HeightBlock]>::to_vec)
            .collect();
        let mut this = Self {
            chunks,
            counts: Vec::new(),
            len: blocks.len(),
        };
        this.retree();
        this
    }
}

impl HeightBlocks {
    fn retree(&mut self) {
        self.counts.clear();
        self.counts.push(0);
        self.counts
            .extend(self.chunks.iter().map(|chunk| chunk.len()));
        for index in 1..self.counts.len() {
            let parent = index + (index & index.wrapping_neg());
            if parent < self.counts.len() {
                self.counts[parent] += self.counts[index];
            }
        }
    }

    fn len(&self) -> usize {
        self.len
    }

    fn clear(&mut self) {
        self.chunks.clear();
        self.counts.clear();
        self.len = 0;
    }

    fn locate(&self, ordinal: usize) -> Option<(usize, usize)> {
        if ordinal >= self.len {
            return None;
        }
        let mut chunk = 0;
        let mut before = 0;
        let mut step = 1;
        while step << 1 < self.counts.len() {
            step <<= 1;
        }
        while step > 0 {
            let next = chunk + step;
            if next < self.counts.len() && before + self.counts[next] <= ordinal {
                chunk = next;
                before += self.counts[next];
            }
            step >>= 1;
        }
        Some((chunk, ordinal - before))
    }

    fn locate_insert(&self, ordinal: usize) -> (usize, usize) {
        self.locate(ordinal).unwrap_or_else(|| {
            self.chunks
                .last()
                .map_or((0, 0), |chunk| (self.chunks.len() - 1, chunk.len()))
        })
    }

    fn get(&self, ordinal: usize) -> Option<HeightBlock> {
        let (chunk, slot) = self.locate(ordinal)?;
        self.chunks.get(chunk)?.get(slot).copied()
    }

    fn range_eq(&self, range: Range<usize>, blocks: &[HeightBlock]) -> bool {
        range.len() == blocks.len()
            && blocks
                .iter()
                .enumerate()
                .all(|(at, block)| self.get(range.start + at).as_ref() == Some(block))
    }

    fn splice(&mut self, range: Range<usize>, blocks: &[HeightBlock]) {
        let (first_chunk, first_slot) = self.locate_insert(range.start);
        let (last_chunk, last_slot) = self.locate_insert(range.end);
        let mut merged = Vec::with_capacity(blocks.len() + 2 * HEIGHT_BLOCK_CHUNK_TARGET);
        if let Some(chunk) = self.chunks.get(first_chunk) {
            merged.extend_from_slice(&chunk[..first_slot.min(chunk.len())]);
        }
        merged.extend_from_slice(blocks);
        if let Some(chunk) = self.chunks.get(last_chunk) {
            merged.extend_from_slice(&chunk[last_slot.min(chunk.len())..]);
        }
        let replacement = merged
            .chunks(HEIGHT_BLOCK_CHUNK_TARGET)
            .map(<[HeightBlock]>::to_vec)
            .collect::<Vec<_>>();
        let end_chunk = (last_chunk + 1).min(self.chunks.len());
        self.chunks
            .splice(first_chunk.min(end_chunk)..end_chunk, replacement);
        self.len = self.len - range.len() + blocks.len();
        self.retree();
    }
}

fn rebase_ordinal_after_splice(
    ordinal: usize,
    anchor_id: Option<BlockId>,
    replaced: Range<usize>,
    inserted: &[HeightBlock],
    new_len: usize,
) -> usize {
    if new_len == 0 {
        return 0;
    }
    if ordinal < replaced.start {
        return ordinal;
    }
    if ordinal >= replaced.end {
        return (ordinal - replaced.len() + inserted.len()).min(new_len - 1);
    }
    anchor_id
        .and_then(|id| inserted.iter().position(|block| block.id == id))
        .map_or(replaced.start.min(new_len - 1), |at| replaced.start + at)
}

#[derive(Clone, Debug)]
struct LayoutCacheEntry {
    layout: BlockLayout,
    font_revision: u64,
}

impl LayoutCacheEntry {
    fn is_valid(&self, width: f32, font_revision: u64, revision: Revision) -> bool {
        self.layout.width == width
            && self.font_revision == font_revision
            && self.layout.revision == revision
    }
}

impl EditorView {
    pub(crate) fn inline_rename_active(&self) -> bool {
        self.inline_rename.is_some()
    }

    pub(crate) fn sidebar_filter_is_focused(&self) -> bool {
        self.sidebar_filter_focused
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

    pub(crate) fn text_input_render_state(&self) -> Option<InlineRenameRenderState> {
        if let Some(state) = self.inline_rename_render_state() {
            return Some(state);
        }
        if self.inline_rename_active() {
            return None;
        }
        Some(InlineRenameRenderState {
            text: self.sidebar_filter.clone(),
            selected_range: if self.sidebar_filter_focused {
                self.sidebar_filter_selected_range.clone()
            } else {
                0..0
            },
            selection_reversed: self.sidebar_filter_selection_reversed,
            marked_range: self
                .sidebar_filter_focused
                .then(|| self.sidebar_filter_marked_range.clone())
                .flatten(),
        })
    }

    pub(crate) fn set_inline_rename_input_bounds(&mut self, bounds: Bounds<Pixels>) {
        self.inline_rename_input_bounds = Some(bounds);
    }

    pub(crate) fn set_text_input_bounds(&mut self, bounds: Bounds<Pixels>) {
        if self.inline_rename_active() {
            self.set_inline_rename_input_bounds(bounds);
        } else if !self.inline_rename_active() {
            self.sidebar_filter_input_bounds = Some(bounds);
        }
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

    pub(crate) fn sidebar_filter_character_index_for_point(
        &self,
        position: gpui::Point<Pixels>,
        window: &mut Window,
    ) -> Option<usize> {
        let bounds = self.sidebar_filter_input_bounds?;
        let state = self.text_input_render_state()?;
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

    pub(crate) fn sidebar_filter_has_composition(&self) -> bool {
        self.sidebar_filter_composition.is_some()
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

    pub(crate) fn sidebar_filter_text_for_range(
        &self,
        range_utf16: Range<usize>,
    ) -> Option<(String, Range<usize>)> {
        let range = byte_range_from_utf16(&self.sidebar_filter, &range_utf16);
        Some((
            self.sidebar_filter[range.clone()].to_owned(),
            range_to_utf16(&self.sidebar_filter, &range),
        ))
    }

    pub(crate) fn sidebar_filter_selection(&self) -> Option<(Range<usize>, bool)> {
        if !self.sidebar_filter_focused {
            return None;
        }
        Some((
            range_to_utf16(&self.sidebar_filter, &self.sidebar_filter_selected_range),
            self.sidebar_filter_selection_reversed,
        ))
    }

    pub(crate) fn sidebar_filter_marked_range(&self) -> Option<Range<usize>> {
        self.sidebar_filter_marked_range
            .as_ref()
            .map(|range| range_to_utf16(&self.sidebar_filter, range))
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

    pub(crate) fn replace_sidebar_filter_text(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.sidebar_filter_focused {
            return false;
        }
        let range = range_utf16
            .as_ref()
            .map(|range| byte_range_from_utf16(&self.sidebar_filter, range))
            .or_else(|| self.sidebar_filter_marked_range.clone())
            .unwrap_or_else(|| self.sidebar_filter_selected_range.clone());
        let replacement: String = new_text
            .chars()
            .filter(|character| *character != '\n' && *character != '\r')
            .collect();
        self.sidebar_filter
            .replace_range(range.clone(), &replacement);
        let next = range.start + replacement.len();
        self.sidebar_filter_selected_range = next..next;
        self.sidebar_filter_selection_reversed = false;
        self.sidebar_filter_marked_range = None;
        self.sidebar_filter_composition = None;
        self.sidebar_filter_changed(cx);
        true
    }

    pub(crate) fn replace_and_mark_sidebar_filter_text(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selected_range_utf16: Option<Range<usize>>,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.sidebar_filter_focused {
            return false;
        }
        if self.sidebar_filter_composition.is_none() {
            self.sidebar_filter_composition = Some(SidebarFilterComposition {
                text: self.sidebar_filter.clone(),
                selected_range: self.sidebar_filter_selected_range.clone(),
                selection_reversed: self.sidebar_filter_selection_reversed,
            });
        }
        let range = range_utf16
            .as_ref()
            .map(|range| byte_range_from_utf16(&self.sidebar_filter, range))
            .or_else(|| self.sidebar_filter_marked_range.clone())
            .unwrap_or_else(|| self.sidebar_filter_selected_range.clone());
        let replacement: String = new_text
            .chars()
            .filter(|character| *character != '\n' && *character != '\r')
            .collect();
        self.sidebar_filter
            .replace_range(range.clone(), &replacement);
        let marked_end = range.start + replacement.len();
        self.sidebar_filter_marked_range =
            (!replacement.is_empty()).then_some(range.start..marked_end);
        self.sidebar_filter_selected_range = inline_rename_selected_range(
            range.start,
            &replacement,
            new_selected_range_utf16,
            marked_end..marked_end,
        );
        self.sidebar_filter_selection_reversed = false;
        self.sidebar_filter_changed(cx);
        true
    }

    pub(crate) fn commit_sidebar_filter_composition(&mut self, cx: &mut Context<Self>) {
        if self.sidebar_filter_composition.take().is_some() {
            self.sidebar_filter_marked_range = None;
            cx.notify();
        }
    }

    pub(crate) fn cancel_sidebar_filter_composition(&mut self, cx: &mut Context<Self>) {
        let Some(composition) = self.sidebar_filter_composition.take() else {
            return;
        };
        self.sidebar_filter = composition.text;
        self.sidebar_filter_selected_range = composition.selected_range;
        self.sidebar_filter_selection_reversed = composition.selection_reversed;
        self.sidebar_filter_marked_range = None;
        self.sidebar_filter_changed(cx);
    }

    pub(crate) fn selected_sidebar_filter_text(&self) -> Option<String> {
        (!self.sidebar_filter_selected_range.is_empty())
            .then(|| self.sidebar_filter[self.sidebar_filter_selected_range.clone()].to_owned())
    }

    pub(crate) fn move_sidebar_filter_left(&mut self, extend: bool, cx: &mut Context<Self>) {
        self.move_sidebar_filter_horizontal(false, extend, cx);
    }

    pub(crate) fn move_sidebar_filter_right(&mut self, extend: bool, cx: &mut Context<Self>) {
        self.move_sidebar_filter_horizontal(true, extend, cx);
    }

    fn move_sidebar_filter_horizontal(
        &mut self,
        right: bool,
        extend: bool,
        cx: &mut Context<Self>,
    ) {
        if !self.sidebar_filter_focused {
            return;
        }
        let cursor = if self.sidebar_filter_selection_reversed {
            self.sidebar_filter_selected_range.start
        } else {
            self.sidebar_filter_selected_range.end
        };
        let target = if right {
            next_inline_rename_boundary(&self.sidebar_filter, cursor)
        } else {
            previous_inline_rename_boundary(&self.sidebar_filter, cursor)
        };
        if extend {
            select_inline_rename_to_fields(
                &mut self.sidebar_filter_selected_range,
                &mut self.sidebar_filter_selection_reversed,
                target,
            );
        } else if self.sidebar_filter_selected_range.is_empty() {
            self.sidebar_filter_selected_range = target..target;
            self.sidebar_filter_selection_reversed = false;
        } else {
            let target = if right {
                self.sidebar_filter_selected_range.end
            } else {
                self.sidebar_filter_selected_range.start
            };
            self.sidebar_filter_selected_range = target..target;
            self.sidebar_filter_selection_reversed = false;
        }
        cx.notify();
    }

    pub(crate) fn select_sidebar_filter_left(&mut self, cx: &mut Context<Self>) {
        self.select_sidebar_filter_to(false, cx);
    }

    pub(crate) fn select_sidebar_filter_right(&mut self, cx: &mut Context<Self>) {
        self.select_sidebar_filter_to(true, cx);
    }

    fn select_sidebar_filter_to(&mut self, right: bool, cx: &mut Context<Self>) {
        if !self.sidebar_filter_focused {
            return;
        }
        let cursor = if self.sidebar_filter_selection_reversed {
            self.sidebar_filter_selected_range.start
        } else {
            self.sidebar_filter_selected_range.end
        };
        let target = if right {
            next_inline_rename_boundary(&self.sidebar_filter, cursor)
        } else {
            previous_inline_rename_boundary(&self.sidebar_filter, cursor)
        };
        select_inline_rename_to_fields(
            &mut self.sidebar_filter_selected_range,
            &mut self.sidebar_filter_selection_reversed,
            target,
        );
        cx.notify();
    }

    pub(crate) fn select_all_sidebar_filter(&mut self, cx: &mut Context<Self>) {
        if self.sidebar_filter_focused {
            self.sidebar_filter_selected_range = 0..self.sidebar_filter.len();
            self.sidebar_filter_selection_reversed = false;
            cx.notify();
        }
    }

    pub(crate) fn select_sidebar_filter_home(&mut self, cx: &mut Context<Self>) {
        if self.sidebar_filter_focused {
            select_inline_rename_to_fields(
                &mut self.sidebar_filter_selected_range,
                &mut self.sidebar_filter_selection_reversed,
                0,
            );
            cx.notify();
        }
    }

    pub(crate) fn select_sidebar_filter_end(&mut self, cx: &mut Context<Self>) {
        if self.sidebar_filter_focused {
            let target = self.sidebar_filter.len();
            select_inline_rename_to_fields(
                &mut self.sidebar_filter_selected_range,
                &mut self.sidebar_filter_selection_reversed,
                target,
            );
            cx.notify();
        }
    }

    pub(crate) fn move_sidebar_filter_home(&mut self, cx: &mut Context<Self>) {
        self.move_sidebar_filter_to(0, cx);
    }

    pub(crate) fn move_sidebar_filter_end(&mut self, cx: &mut Context<Self>) {
        let target = self.sidebar_filter.len();
        self.move_sidebar_filter_to(target, cx);
    }

    fn move_sidebar_filter_to(&mut self, target: usize, cx: &mut Context<Self>) {
        if self.sidebar_filter_focused {
            self.sidebar_filter_selected_range = target..target;
            self.sidebar_filter_selection_reversed = false;
            cx.notify();
        }
    }

    pub(crate) fn backspace_sidebar_filter(&mut self, cx: &mut Context<Self>) {
        self.delete_sidebar_filter_with_direction(true, cx);
    }

    pub(crate) fn delete_sidebar_filter(&mut self, cx: &mut Context<Self>) {
        self.delete_sidebar_filter_with_direction(false, cx);
    }

    fn delete_sidebar_filter_with_direction(&mut self, backwards: bool, cx: &mut Context<Self>) {
        if !self.sidebar_filter_focused {
            return;
        }
        let range = if self.sidebar_filter_selected_range.is_empty() {
            let cursor = if self.sidebar_filter_selection_reversed {
                self.sidebar_filter_selected_range.start
            } else {
                self.sidebar_filter_selected_range.end
            };
            if backwards {
                previous_inline_rename_boundary(&self.sidebar_filter, cursor)..cursor
            } else {
                cursor..next_inline_rename_boundary(&self.sidebar_filter, cursor)
            }
        } else {
            self.sidebar_filter_selected_range.clone()
        };
        self.sidebar_filter.replace_range(range.clone(), "");
        self.sidebar_filter_selected_range = range.start..range.start;
        self.sidebar_filter_selection_reversed = false;
        self.sidebar_filter_marked_range = None;
        self.sidebar_filter_composition = None;
        self.sidebar_filter_changed(cx);
    }

    fn sidebar_filter_changed(&mut self, cx: &mut Context<Self>) {
        self.sidebar_scroll.set_offset(point(px(0.0), px(0.0)));
        cx.notify();
    }

    fn focus_sidebar_filter(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.cancel_inline_rename(cx) {
            return;
        }
        let was_focused = self.sidebar_filter_focused;
        self.sidebar_filter_focused = true;
        self.sidebar_keyboard_focus = true;
        window.focus(&self.focus_handle);
        if was_focused {
            if let Some(index) =
                self.sidebar_filter_character_index_for_point(event.position, window)
            {
                let byte = byte_offset_from_utf16(&self.sidebar_filter, index);
                self.sidebar_filter_selected_range = byte..byte;
                self.sidebar_filter_selection_reversed = false;
            }
        } else {
            self.sidebar_filter_selected_range = 0..self.sidebar_filter.len();
            self.sidebar_filter_selection_reversed = false;
        }
        cx.notify();
    }

    pub(crate) fn blur_sidebar_filter(&mut self, cx: &mut Context<Self>) {
        if self.sidebar_filter_focused {
            self.sidebar_filter_focused = false;
            self.sidebar_filter_marked_range = None;
            self.sidebar_filter_composition = None;
            self.sidebar_keyboard_focus = false;
            cx.notify();
        }
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

    fn begin_inline_rename(
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
        window.focus(&self.focus_handle);
        cx.notify();
    }

    fn inline_rename_has_background_conflict(&self, from: &Path, kind: InlineRenameKind) -> bool {
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

    pub fn new(text: &str, file_label: impl Into<String>, cx: &mut Context<Self>) -> Self {
        Self::from_sessions(
            SessionSet::with_untitled(text, file_label),
            Arc::new(OsFileService),
            StateStores::from_environment(),
            cx,
        )
    }

    fn from_sessions(
        sessions: SessionSet,
        files: Arc<dyn FileService>,
        stores: StateStores,
        cx: &mut Context<Self>,
    ) -> Self {
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
        // The caret's mode badge (see `caret_input_mode`) has to
        // update the moment the user switches IME mode, even if nothing else
        // about the document changes, so this reuses the same platform event
        // GPUI already refreshes its keyboard mapper from — no separate
        // polling.
        let input_mode_subscription = cx.on_keyboard_layout_change(|view: &mut Self, cx| {
            view.caret_input_mode = gpui::active_keyboard_input_mode();
            cx.notify();
        });
        // Re-observes the local date on a timer so the sidebar's `本日`
        // badge moves on even when the window sits open, focused, and
        // untouched across local midnight; `view.update` failing (the view
        // has been dropped) ends the loop instead of polling forever.
        let date_badge_refresh_task = cx.spawn(async move |view, cx| {
            loop {
                gpui::Timer::after(DATE_BADGE_REFRESH_INTERVAL).await;
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
            recent,
            work_folder: None,
            draft_store: Arc::new(OsDraftStore),
            work_folder_drafts: HashMap::new(),
            selected_folder: None,
            sidebar_focus: SidebarFocus::ActiveSession,
            expanded_folders: HashSet::new(),
            sidebar_filter: String::new(),
            sidebar_filter_selected_range: 0..0,
            sidebar_filter_selection_reversed: false,
            sidebar_filter_marked_range: None,
            sidebar_filter_composition: None,
            sidebar_filter_focused: false,
            sidebar_filter_input_bounds: None,
            sidebar_keyboard_focus: false,
            inline_rename: None,
            sidebar_width: theme.sidebar_width,
            sidebar_resize_drag: None,
            sidebar_scroll: ScrollHandle::new(),
            sidebar_scrollbar_drag: None,
            editor_scrollbar_drag: None,
            text_selection_drag: false,
            text_autoscroll: None,
            text_autoscroll_activity: 0,
            sidebar_scrollbar_visible: false,
            sidebar_scrollbar_activity: 0,
            sidebar_date_badge_today: local_today(),
            _date_badge_refresh_task: date_badge_refresh_task,
            pending_new_folders: HashSet::new(),
            inline_rename_input_bounds: None,
            _quit_subscription: quit_subscription,
            caret_input_mode: gpui::active_keyboard_input_mode(),
            _input_mode_subscription: input_mode_subscription,
            _input_mode_focus_subscription: None,
            draft_recovery_warning: None,
            title_sync_pending: HashMap::new(),
            title_sync_in_flight: HashSet::new(),
            title_sync_scheduled: HashMap::new(),
            title_sync_deferred: HashSet::new(),
            loading_paths: HashSet::new(),
            latest_open_target: None,
            work_folder_generation: 0,
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
            pending_zoom_anchor: None,
            caret_geometry: None,
            pending_caret_visibility_after_layout: false,
            pending_list_editing: None,
            block_index: BlockIndexState::new(),
            granularity: Granularity::Lines,
            height_blocks: HeightBlocks::default(),
            document_parse_job_running: false,
            last_background_height_disclosure: None,
            last_applied_height_disclosure: None,
            joined_parse_cache: HashMap::new(),
            joined_parse_jobs: HashMap::new(),
            joined_parse_jobs_running: 0,
        }
    }

    /// Opens `path` as the first editor session.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when the document or persisted state cannot be read.
    pub fn open(path: &Path, cx: &mut Context<Self>) -> std::io::Result<Self> {
        #[cfg(any(feature = "instrument", feature = "timing-probe"))]
        let started = Instant::now();
        let files: Arc<dyn FileService> = Arc::new(OsFileService);
        // The first document is read before the window exists, so this one read
        // is synchronous by construction; every later one goes to a thread.
        let loaded = files.load(path)?;
        #[cfg_attr(
            not(any(feature = "instrument", feature = "timing-probe")),
            allow(unused_mut)
        )]
        let mut view = Self::from_sessions(
            SessionSet::with_loaded(loaded),
            files,
            StateStores::from_environment(),
            cx,
        );
        view.remember_recent(path);
        cx.add_recent_document(path);
        #[cfg(any(feature = "instrument", feature = "timing-probe"))]
        {
            view.instrumentation.file_open_time = started.elapsed();
            view.instrumentation.load_rss_bytes = hane_metrics::process_memory_bytes();
        }
        Ok(view)
    }

    /// Opens `root` as a work folder. The window is created immediately with
    /// an empty untitled session; the directory scan and the first note's
    /// load both happen on a background thread, so a folder with many entries
    /// or a large first document never delays the window from appearing, the
    /// same way every later switch does not block on I/O.
    pub fn open_work_folder(root: &Path, cx: &mut Context<Self>) -> Self {
        let mut view = Self::new("", "Untitled", cx);
        view.begin_work_folder_scan(root.to_path_buf(), cx);
        view
    }

    fn begin_work_folder_scan(&mut self, root: PathBuf, cx: &mut Context<Self>) {
        self.status = Some("Opening work folder…".to_owned());
        let draft_store = self.draft_store.clone();
        cx.spawn(async move |view, cx| {
            let scan_root = root.clone();
            let scanned = cx
                .background_executor()
                .spawn(async move {
                    let work_folder = OsWorkFolderScanner.scan(&scan_root);
                    let drafts = draft_store.recover(&scan_root);
                    (work_folder, drafts)
                })
                .await;
            let _ = view.update(cx, |view, cx| view.finish_work_folder_scan(scanned, cx));
        })
        .detach();
        cx.notify();
    }

    fn finish_work_folder_scan(
        &mut self,
        scanned: (
            std::io::Result<WorkFolder>,
            std::io::Result<RecoveredDrafts>,
        ),
        cx: &mut Context<Self>,
    ) {
        let (work_folder, drafts) = scanned;
        match work_folder {
            Err(error) => {
                self.status = Some(format!("Could not open work folder: {error}"));
            }
            Ok(work_folder) => {
                let first = work_folder
                    .entries()
                    .first()
                    .map(|entry| entry.path().to_path_buf());
                // The window still holds the empty untitled session `new`
                // created above; it stays clean and in place while any
                // recovered drafts are installed alongside it, so switching
                // to the first real note below can still reuse it.
                let root = work_folder.root().to_path_buf();
                let initial_session = self.sessions.active_id();
                self.work_folder = Some(work_folder);

                // A recovery failure must not look like the drafts were
                // simply gone: `OsDraftStore::recover` already keeps
                // whatever individual files it could read, so surface the
                // error (or, short of that, how many entries could not be
                // read) instead of silently falling back to an empty list
                // and clearing status as if nothing happened. This goes into
                // `draft_recovery_warning`, not `status`: opening the first
                // note below drives `status` through "Opening…" and
                // "Opened" in this same update cycle, which would otherwise
                // blank the warning out before it was ever shown.
                let mut last_recovered = None;
                self.draft_recovery_warning = match drafts {
                    Ok(drafts) => {
                        for draft in drafts.drafts {
                            let id = self.sessions.open_untitled(&draft.text, "Untitled");
                            // The journal does not record which folder a
                            // draft was destined for, so a recovered draft
                            // falls back to the work folder root rather than
                            // wherever it was selected before the crash.
                            self.work_folder_drafts.insert(
                                id,
                                WorkFolderDraft {
                                    draft_id: draft.id,
                                    target_directory: root.clone(),
                                },
                            );
                            last_recovered = Some(id);
                        }
                        (drafts.failed > 0).then(|| {
                            format!(
                                "{} unsaved draft{} could not be recovered",
                                drafts.failed,
                                if drafts.failed == 1 { "" } else { "s" }
                            )
                        })
                    }
                    Err(error) => Some(format!("Could not recover unsaved drafts: {error}")),
                };

                if let Some(path) = first {
                    // Opening the first entry replaces whichever session is
                    // active through the same background-loading path as any
                    // other open, instead of leaving a spare empty session
                    // around; that must be the original clean one, not
                    // whichever draft was installed last.
                    self.sessions.activate(initial_session);
                    self.open_path(&path, cx);
                } else if last_recovered.is_some() {
                    self.on_document_replaced();
                    self.schedule_document_parse(cx);
                }
            }
        }
        cx.notify();
    }

    #[must_use]
    pub fn editor(&self) -> &Editor {
        self.sessions.active().editor()
    }

    pub(crate) fn editor_mut(&mut self) -> &mut Editor {
        self.sessions.active_mut().editor_mut()
    }

    /// The open documents, for a tab strip or filer to render.
    pub fn sessions(&self) -> impl Iterator<Item = &DocumentSession> {
        self.sessions.sessions()
    }

    #[must_use]
    pub fn active_session(&self) -> &DocumentSession {
        self.sessions.active()
    }

    /// Switches to another open document, carrying the current one's scroll
    /// position with it and rebuilding everything derived from the document.
    fn active_session_has_sidebar_row(&self) -> bool {
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

    pub fn activate_session(&mut self, id: SessionId, cx: &mut Context<Self>) -> bool {
        if id == self.sessions.active_id() {
            self.sidebar_focus = SidebarFocus::ActiveSession;
            cx.notify();
            return true;
        }
        let scroll_y = self.scroll_y;
        self.sessions
            .active_mut()
            .set_view_state(SessionViewState { scroll_y });
        if !self.sessions.activate(id) {
            return false;
        }
        self.on_document_replaced();
        self.schedule_document_parse(cx);
        cx.notify();
        true
    }

    fn document_key(&self) -> DocumentKey {
        DocumentKey {
            session: self.sessions.active_id(),
            generation: self.sessions.active().generation(),
        }
    }

    /// Cancels a selection drag before the state for another document is
    /// installed. The timer may already be sleeping, so bump its activity as
    /// well; its next tick will observe the stale activity and exit without
    /// touching the replacement document.
    fn cancel_text_selection_autoscroll(&mut self) {
        self.text_selection_drag = false;
        self.text_autoscroll = None;
        self.text_autoscroll_activity = self.text_autoscroll_activity.wrapping_add(1);
    }

    /// Rebuilds the view state that only makes sense for one document instance.
    fn on_document_replaced(&mut self) {
        self.cancel_text_selection_autoscroll();
        // Whatever the sidebar had highlighted before, the document on
        // screen just changed to a specific file or draft, so that is what
        // should be highlighted now instead.
        self.sidebar_focus = SidebarFocus::ActiveSession;
        let lines = self.sessions.active().editor().document().line_count();
        self.granularity = Granularity::Lines;
        self.heights = HeightIndex::new(std::iter::repeat_n(self.line_height(), lines));
        self.height_blocks.clear();
        self.scroll_y = self.sessions.active().view_state().scroll_y;
        self.block_cache.clear();
        self.line_owners.clear();
        self.layout_cache.clear();
        self.caret_geometry = None;
        self.pending_caret_visibility_after_layout = false;
        self.pending_list_editing = None;
        self.block_index = BlockIndexState::new();
        self.last_background_height_disclosure = None;
        self.last_applied_height_disclosure = None;
        // A `BlockId` is only unique within the document it was assigned by;
        // a cached whole-span parse keyed by one could otherwise be reused
        // for an unrelated block in whatever document replaced it.
        self.joined_parse_cache.clear();
        self.joined_parse_jobs.clear();
    }

    fn remember_recent(&mut self, path: &Path) {
        self.recent.remember(path);
        if let Err(error) = self.stores.recent_files().store(&self.recent) {
            self.status = Some(format!("Recent files failed: {error}"));
        }
    }

    fn store_settings(&mut self) {
        if let Err(error) = self.stores.settings().store(&self.settings) {
            self.status = Some(format!("Settings failed: {error}"));
        }
    }

    /// Block that owns a physical source line, with the confidence the index has
    /// in it. `None` while no index is published yet, or for a line in a document
    /// that holds no Markdown block at all.
    #[must_use]
    pub fn block_at_line(&self, line: usize) -> Option<IndexedBlock> {
        block_at_line(self.block_index.index()?, self.editor().document(), line)
    }

    pub(crate) fn after_input(&mut self, cx: &mut Context<Self>) {
        // Keep block boundaries current without waiting for the background parse:
        // this re-parses only the edited window, never the document.
        if let Some(update) = self
            .block_index
            .apply_edits(self.sessions.active().editor().document())
        {
            self.record_block_index_update(&update);
            if !self.apply_height_index_update(&update) {
                self.resync_heights();
            }
        } else {
            self.resync_heights();
        }
        self.ensure_active_disclosure_height();
        self.scroll_cursor_into_view();
        // The command may disclose markup and change its row height only on
        // the next render. Re-check once after that layout is installed so
        // the caret and its input-mode badge use current geometry.
        self.pending_caret_visibility_after_layout = true;
        self.schedule_document_parse(cx);
        self.schedule_autosave(cx);
        self.schedule_draft_save(cx);
        self.schedule_title_sync(cx);
        cx.notify();
    }

    /// Arms the debounce timer for the active session. Each call invalidates the
    /// timer armed by the previous keystroke, so a burst of typing produces one
    /// write at the end rather than one per key.
    fn schedule_autosave(&mut self, cx: &mut Context<Self>) {
        let autosave = self.settings.autosave;
        let session = self.sessions.active_mut();
        session.note_edit();
        let Some(ticket) = session.autosave_ticket(autosave) else {
            return;
        };
        let id = session.id();
        cx.spawn(async move |view, cx| {
            gpui::Timer::after(Duration::from_millis(750)).await;
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
    fn schedule_draft_save(&mut self, cx: &mut Context<Self>) {
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
            gpui::Timer::after(Duration::from_millis(750)).await;
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
    fn schedule_title_sync(&mut self, cx: &mut Context<Self>) {
        if self.work_folder.is_none() {
            return;
        }
        let id = self.sessions.active_id();
        let generation = self.sessions.active().generation();
        let revision = self.sessions.active().revision();
        self.title_sync_scheduled.insert(id, revision);
        cx.spawn(async move |view, cx| {
            gpui::Timer::after(Duration::from_millis(750)).await;
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
        cx.notify();
    }

    /// The directory a new note or folder should be created in: the selected
    /// sidebar folder if one is selected and still exists in the tree,
    /// otherwise the work folder root. `None` outside a work folder.
    fn target_directory_for_new_entry(&self) -> Option<PathBuf> {
        let work_folder = self.work_folder.as_ref()?;
        match &self.selected_folder {
            Some(selected) if work_folder.children_at(selected).is_some() => Some(selected.clone()),
            _ => Some(work_folder.root().to_path_buf()),
        }
    }

    /// Issue #5: starts a brand-new, unnamed note in the current work folder.
    /// No filename prompt: it opens blank and ready for input immediately,
    /// and is journalled into the recovery drafts as soon as it holds
    /// anything, so a crash before it earns a real name never loses it.
    pub fn new_work_folder_note(&mut self, cx: &mut Context<Self>) {
        if !self.cancel_inline_rename(cx) {
            return;
        }
        self.blur_sidebar_filter(cx);
        self.sidebar_keyboard_focus = false;
        let Some(target_directory) = self.target_directory_for_new_entry() else {
            return;
        };
        let scroll_y = self.scroll_y;
        self.sessions
            .active_mut()
            .set_view_state(SessionViewState { scroll_y });
        let id = self.sessions.open_untitled("", "Untitled");
        self.work_folder_drafts.insert(
            id,
            WorkFolderDraft {
                draft_id: DraftId::generate(),
                target_directory,
            },
        );
        self.on_document_replaced();
        self.schedule_document_parse(cx);
        self.status = None;
        cx.notify();
    }

    /// Toggles a sidebar folder's expanded state and makes it the selected
    /// target directory for the next new note or folder, both on the same
    /// click: there is no separate disclosure control in this tree.
    fn toggle_and_select_work_folder_folder(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        if !self.cancel_inline_rename(cx) {
            return;
        }
        self.blur_sidebar_filter(cx);
        self.sidebar_keyboard_focus = true;
        if self.expanded_folders.contains(&path) {
            self.expanded_folders.remove(&path);
        } else {
            self.expanded_folders.insert(path.clone());
        }
        self.selected_folder = Some(path);
        self.sidebar_focus = SidebarFocus::Folder;
        cx.notify();
    }

    /// Selects the work folder root as the target directory for the next new
    /// note or folder. The root row has no expanded state of its own: its
    /// children are always shown, so unlike a subfolder's row this only ever
    /// selects, never toggles.
    fn select_work_folder_root(&mut self, cx: &mut Context<Self>) {
        if !self.cancel_inline_rename(cx) {
            return;
        }
        self.blur_sidebar_filter(cx);
        self.sidebar_keyboard_focus = true;
        self.selected_folder = None;
        self.sidebar_focus = SidebarFocus::Folder;
        cx.notify();
    }

    /// Creates a new, empty subfolder inside the selected sidebar folder (the
    /// work folder root if nothing is selected), named "New Folder" or, on a
    /// collision with an existing sibling, "New Folder 2", "New Folder 3", …
    /// Creation goes through `FileService::create_dir`, the filesystem
    /// boundary, rather than touching `std::fs` from the UI directly. Once
    /// created, the folder is added to the tree and shown expanded, so it is
    /// immediately visible without waiting for a rescan.
    pub fn new_work_folder_folder(&mut self, cx: &mut Context<Self>) {
        if !self.cancel_inline_rename(cx) {
            return;
        }
        self.blur_sidebar_filter(cx);
        self.sidebar_keyboard_focus = false;
        let Some(work_folder) = self.work_folder.as_ref() else {
            return;
        };
        let Some(target_directory) = self.target_directory_for_new_entry() else {
            return;
        };
        let siblings = work_folder
            .children_at(&target_directory)
            .unwrap_or_default();
        let pending = &self.pending_new_folders;
        let name = unique_folder_name("New Folder", |candidate| {
            siblings.iter().any(|node| match node {
                WorkFolderNode::Folder(folder) => folder.name() == candidate,
                WorkFolderNode::File(_) => false,
            }) || pending.contains(&target_directory.join(candidate))
        });
        let path = target_directory.join(&name);
        // Reserved synchronously, before any `await`: the tree is not
        // updated until `create_dir` finishes in the background, so a second
        // "new folder" click landing before this one completes would
        // otherwise see the same (empty) sibling list and pick the same
        // name, silently losing one of the two folders since `create_dir`
        // succeeds unconditionally on an already-existing directory.
        self.pending_new_folders.insert(path.clone());
        let files = self.files.clone();
        let probe_path = path.clone();
        cx.spawn(async move |view, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { files.create_dir(&probe_path) })
                .await;
            let _ = view.update(cx, |view, cx| {
                view.finish_new_work_folder_folder(path, result, cx);
            });
        })
        .detach();
    }

    fn finish_new_work_folder_folder(
        &mut self,
        path: PathBuf,
        result: std::io::Result<()>,
        cx: &mut Context<Self>,
    ) {
        self.pending_new_folders.remove(&path);
        match result {
            Ok(()) => {
                if let Some(folder) = self.work_folder.as_mut() {
                    folder.insert_folder(path.clone());
                }
                self.expanded_folders.insert(path);
                self.status = None;
            }
            Err(error) => {
                self.status = Some(format!("New Folder failed: {error}"));
            }
        }
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
    fn retry_deferred_title_sync(&mut self, cx: &mut Context<Self>) {
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
    fn save_session(&mut self, id: SessionId, intent: SaveIntent, cx: &mut Context<Self>) {
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
                cx.spawn(async move |view, cx| {
                    let result = cx
                        .background_executor()
                        .spawn(async move {
                            run_save_job(files.as_ref(), &job.path, &job.document, job.guard)
                        })
                        .await;
                    let _ = view.update(cx, |view, cx| {
                        view.finish_save(id, ticket, &path, result, cx);
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
        path: &Path,
        result: Result<SavedFile, SaveFailure>,
        cx: &mut Context<Self>,
    ) {
        let Some(outcome) = self
            .sessions
            .get_mut(id)
            .map(|session| session.finish_save(ticket, result))
        else {
            return;
        };
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

    pub(crate) fn prompt_open(&mut self, cx: &mut Context<Self>) {
        if self.sessions.active().is_dirty() {
            self.status = Some("Save current changes before opening another file".to_owned());
            cx.notify();
            return;
        }
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Open Markdown".into()),
        });
        cx.spawn(async move |view, cx| match receiver.await {
            Ok(Ok(Some(paths))) => {
                if let Some(path) = paths.into_iter().next() {
                    let _ = view.update(cx, |view, cx| view.open_path(&path, cx));
                }
            }
            Ok(Err(error)) => {
                let _ = view.update(cx, |view, cx| {
                    view.status = Some(format!("Open failed: {error}"));
                    cx.notify();
                });
            }
            _ => {}
        })
        .detach();
    }

    /// Whether any open session — not just the active one — has unsaved
    /// edits. The work-folder sidebar deliberately keeps more than one
    /// session open at a time (`open_work_folder_entry` gives every visited
    /// note its own session), so a dirty background session is exactly as
    /// real a reason to block a switch as a dirty active one.
    fn has_unsaved_sessions(&self) -> bool {
        self.sessions.sessions().any(DocumentSession::is_dirty)
    }

    /// Prompts for a directory and switches this window into work-folder mode
    /// on it, the GUI entry point for what a startup CLI argument already
    /// does through `EditorView::open_work_folder`. Guards on dirty state the
    /// same way `prompt_open` does: switching folders discards whichever
    /// sessions are open in this window.
    pub(crate) fn prompt_open_work_folder(&mut self, cx: &mut Context<Self>) {
        if self.has_unsaved_sessions() {
            self.status = Some("Save current changes before opening a folder".to_owned());
            cx.notify();
            return;
        }
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Open Work Folder".into()),
        });
        cx.spawn(async move |view, cx| match receiver.await {
            Ok(Ok(Some(paths))) => {
                if let Some(root) = paths.into_iter().next() {
                    let _ = view.update(cx, |view, cx| {
                        // Re-checked here, not just before the (blocking,
                        // but still async from GPUI's perspective) picker
                        // was shown: an autosave or draft-save timer armed
                        // before the prompt could still land while it was
                        // open, and a background session's edit is not
                        // guarded by anything else in between.
                        if view.has_unsaved_sessions() {
                            view.status =
                                Some("Save current changes before opening a folder".to_owned());
                            cx.notify();
                        } else {
                            view.switch_to_work_folder(root, cx);
                        }
                    });
                }
            }
            Ok(Err(error)) => {
                let _ = view.update(cx, |view, cx| {
                    view.status = Some(format!("Open Folder failed: {error}"));
                    cx.notify();
                });
            }
            _ => {}
        })
        .detach();
    }

    /// First-run startup prompt, shown when no default folder is configured
    /// yet. Unlike `prompt_open_work_folder`, the chosen directory is saved
    /// as the default folder so ordinary launches open it automatically from
    /// then on; a folder passed on the command line (e.g. Explorer's "Open
    /// with Hane") never goes through this path, so it never changes the
    /// default. No unsaved-session guard is needed here: this only runs
    /// against the fresh untitled window startup creates before any prompt.
    pub fn prompt_default_work_folder(&mut self, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Choose a Default Folder".into()),
        });
        cx.spawn(async move |view, cx| match receiver.await {
            Ok(Ok(Some(paths))) => {
                if let Some(root) = paths.into_iter().next() {
                    let _ = view.update(cx, |view, cx| {
                        view.settings.default_folder = Some(root.clone());
                        view.store_settings();
                        view.switch_to_work_folder(root, cx);
                    });
                }
            }
            Ok(Err(error)) => {
                let _ = view.update(cx, |view, cx| {
                    view.status = Some(format!("Choose Default Folder failed: {error}"));
                    cx.notify();
                });
            }
            _ => {}
        })
        .detach();
    }

    /// Resets this window to a single fresh untitled session and begins
    /// scanning `root` as a work folder — the runtime equivalent of the
    /// startup path through `open_work_folder`. Every previously open
    /// session, and any state keyed by its id, is discarded rather than kept
    /// around: a work folder switch is a new window's worth of state, not
    /// another document alongside the old ones.
    fn switch_to_work_folder(&mut self, root: PathBuf, cx: &mut Context<Self>) {
        // Whatever `schedule_draft_save` still owed the old folder's unnamed
        // notes must land before their `work_folder_drafts` entries are
        // dropped below, or the same debounce gap `flush_pending_drafts` was
        // added to close on quit would reopen here.
        self.flush_pending_drafts();
        self.sessions = SessionSet::with_untitled("", "Untitled");
        self.work_folder = None;
        self.work_folder_drafts.clear();
        self.selected_folder = None;
        self.expanded_folders.clear();
        self.sidebar_filter.clear();
        self.sidebar_filter_selected_range = 0..0;
        self.sidebar_filter_selection_reversed = false;
        self.sidebar_filter_marked_range = None;
        self.sidebar_filter_composition = None;
        self.sidebar_filter_focused = false;
        self.sidebar_filter_input_bounds = None;
        self.pending_new_folders.clear();
        self.draft_recovery_warning = None;
        self.title_sync_pending.clear();
        self.title_sync_in_flight.clear();
        self.title_sync_scheduled.clear();
        self.title_sync_deferred.clear();
        self.loading_paths.clear();
        self.latest_open_target = None;
        // Any background read still in flight for the old folder (or for
        // single-file state) is now for a session that no longer exists;
        // bumping this makes `finish_open` discard it instead of merging a
        // stale file into the sessions just installed above.
        self.work_folder_generation = self.work_folder_generation.wrapping_add(1);
        self.on_document_replaced();
        self.begin_work_folder_scan(root, cx);
    }

    /// Opens a path the way a filer will: the session set decides whether this
    /// is a switch, a load, or a refusal, and the read itself happens on a
    /// background thread so a large file never blocks typing.
    pub fn open_path(&mut self, path: &Path, cx: &mut Context<Self>) {
        if !self.cancel_inline_rename(cx) {
            return;
        }
        self.blur_sidebar_filter(cx);
        self.sidebar_keyboard_focus = false;
        self.open_with_policy(path, OpenPolicy::ReuseActive, cx);
    }

    /// Opens a work folder sidebar entry: an already-open note is reused, and
    /// an unloaded one is loaded into a session of its own, so switching notes
    /// never asks the user to save whatever else happens to be open.
    pub fn open_work_folder_entry(&mut self, path: &Path, cx: &mut Context<Self>) {
        if !self.cancel_inline_rename(cx) {
            return;
        }
        self.blur_sidebar_filter(cx);
        self.sidebar_keyboard_focus = true;
        self.open_with_policy(path, OpenPolicy::NewSession, cx);
    }

    fn open_with_policy(&mut self, path: &Path, policy: OpenPolicy, cx: &mut Context<Self>) {
        // Whichever path was asked for most recently is what the user wants
        // to see; a load that lands after a newer request must not steal
        // focus back to what it was asked for.
        self.latest_open_target = Some(path.to_path_buf());
        match self.sessions.open_decision(path, policy) {
            OpenDecision::Reject(_) => {
                self.status = Some("Save current changes before opening another file".to_owned());
                cx.notify();
            }
            OpenDecision::Activate(id) => {
                self.activate_session(id, cx);
                self.status = Some("Already open".to_owned());
                cx.notify();
            }
            OpenDecision::Load { into } => {
                if !self.loading_paths.insert(path.to_path_buf()) {
                    // Already loading this path: the in-flight read will
                    // apply once it lands, and `latest_open_target` above is
                    // enough to make it win if nothing newer was requested
                    // meanwhile. Starting a second read here is what used to
                    // produce two sessions for one file.
                    cx.notify();
                    return;
                }
                self.status = Some("Opening…".to_owned());
                let files = self.files.clone();
                let path = path.to_path_buf();
                let generation = self.work_folder_generation;
                cx.spawn(async move |view, cx| {
                    let loaded = cx
                        .background_executor()
                        .spawn({
                            let path = path.clone();
                            async move { files.load(&path) }
                        })
                        .await;
                    let _ = view.update(cx, |view, cx| {
                        view.finish_open(into, generation, &path, loaded, cx);
                    });
                })
                .detach();
                cx.notify();
            }
        }
    }

    fn finish_open(
        &mut self,
        into: Option<SessionId>,
        generation: u64,
        path: &Path,
        loaded: std::io::Result<LoadedFile>,
        cx: &mut Context<Self>,
    ) {
        self.loading_paths.remove(path);
        if generation != self.work_folder_generation {
            // `switch_to_work_folder` replaced `sessions`/`work_folder` while
            // this read was in flight. `into` and `latest_open_target` only
            // protect against staleness within one folder (or single-file
            // state); neither can tell this read apart from a same-path
            // request made after the switch, so the generation counter is
            // what keeps a file that belongs to a folder that is no longer
            // open from being merged into the one that replaced it.
            cx.notify();
            return;
        }
        match loaded {
            Err(error) => self.status = Some(format!("Open failed: {error}")),
            Ok(loaded) => {
                // The read took time, and the target session may have been
                // edited in the meantime: re-check before replacing it.
                if into
                    .is_some_and(|id| self.sessions.get(id).is_some_and(DocumentSession::is_dirty))
                {
                    self.status =
                        Some("Save current changes before opening another file".to_owned());
                } else {
                    // A newer request may have been made and even resolved
                    // while this one was in flight; only the load that still
                    // matches what was most recently asked for is allowed to
                    // take over what is on screen.
                    let is_latest_request = self.latest_open_target.as_deref() == Some(path);
                    let previously_active = self.sessions.active_id();
                    // `ReuseActive` always targets whatever session was active
                    // when the request was made, so a stale completion here
                    // can name the very session a newer, already-applied
                    // completion put on screen. Overwriting it would corrupt
                    // what the user is now looking at with no way back, so a
                    // stale result that targets the current active session is
                    // discarded instead of applied.
                    if !is_latest_request && into == Some(previously_active) {
                        self.status =
                            Some("A newer document is open; this load was discarded".to_owned());
                    } else {
                        if is_latest_request {
                            let scroll_y = self.scroll_y;
                            self.sessions
                                .active_mut()
                                .set_view_state(SessionViewState { scroll_y });
                        }
                        self.sessions.apply_open(into, loaded);
                        self.remember_recent(path);
                        cx.add_recent_document(path);
                        if is_latest_request {
                            self.on_document_replaced();
                            self.status = Some("Opened".to_owned());
                            self.schedule_document_parse(cx);
                        } else {
                            // The session now holds the loaded document and is
                            // ready to be reused instantly next time it is
                            // selected, but it must not visibly replace
                            // whatever the user has since switched to.
                            self.sessions.activate(previously_active);
                        }
                    }
                }
            }
        }
        cx.notify();
    }

    pub(crate) fn toggle_autosave(&mut self, cx: &mut Context<Self>) {
        self.settings.autosave = !self.settings.autosave;
        self.status = Some(format!(
            "Autosave {}",
            if self.settings.autosave { "on" } else { "off" }
        ));
        self.store_settings();
        self.schedule_autosave(cx);
        cx.notify();
    }

    pub(crate) fn cycle_theme(&mut self, window: &Window, cx: &mut Context<Self>) {
        self.settings.theme = self.settings.theme.next();
        self.theme = resolve_theme(self.settings.theme, window.appearance());
        self.block_cache.clear();
        self.layout_cache.clear();
        self.heights = HeightIndex::new(self.item_heights());
        self.store_settings();
        cx.notify();
    }

    /// Coalesced background job producing the formal, document-wide `BlockIndex`.
    /// One job at a time; a result that no longer matches the document revision
    /// is rebased or re-scheduled rather than published stale.
    fn schedule_document_parse(&mut self, cx: &mut Context<Self>) {
        let document = self.sessions.active().editor().document();
        let revision = document.revision();
        let disclosure = self.active_height_disclosure();
        let disclosure_refresh = disclosure
            .filter(|disclosure| !disclosure.is_empty())
            .is_some_and(|disclosure| {
                self.last_background_height_disclosure != Some((revision, disclosure))
            });
        let disclosure_collapse = disclosure
            .filter(|disclosure| disclosure.is_empty())
            .is_some_and(|_| {
                self.last_background_height_disclosure
                    .is_some_and(|(background_revision, background)| {
                        background_revision == revision && !background.is_empty()
                    })
            });
        if !self.block_index.needs_formal_parse(document)
            && !disclosure_refresh
            && !disclosure_collapse
        {
            return;
        }
        if self.document_parse_job_running {
            return;
        }
        self.document_parse_job_running = true;
        let key = self.document_key();
        let line_height = self.line_height();
        let line_height_bits = line_height.to_bits();
        if let Some(disclosure) = disclosure {
            self.last_background_height_disclosure = Some((revision, disclosure));
        }
        let snapshot = self.editor().document().clone();
        cx.spawn(async move |view, cx| {
            gpui::Timer::after(Duration::from_millis(40)).await;
            let current = view
                .update(cx, |view, _| {
                    view.document_key() == key
                        && block_context_revision_is_current(
                            view.editor().document().revision(),
                            revision,
                        )
                        && height_snapshot_matches_line_height(
                            view.line_height(),
                            f32::from_bits(line_height_bits),
                        )
                })
                .unwrap_or(false);
            if !current {
                let _ = view.update(cx, |view, cx| {
                    view.document_parse_job_running = false;
                    view.schedule_document_parse(cx);
                });
                return;
            }
            // Sizing the height index is proportional to the block count, so it
            // is done here rather than on the main thread: for a 100 MB document
            // that is tens of milliseconds that would otherwise land in one
            // frame.
            let (index, heights) = cx
                .background_executor()
                .spawn(async move {
                    let index = BlockIndex::from_buffer(&snapshot);
                    let heights = HeightIndex::new(block_heights_with_disclosure(
                        &snapshot,
                        &index,
                        line_height,
                        disclosure,
                    ));
                    (index, heights)
                })
                .await;
            let _ = view.update(cx, |view, cx| {
                view.document_parse_job_running = false;
                if view.document_key() != key
                    || !height_snapshot_matches_line_height(
                        view.line_height(),
                        f32::from_bits(line_height_bits),
                    )
                {
                    // The index itself may still be current, but these
                    // heights were measured for an older zoom/theme. Keep the
                    // stale snapshot out of the visible height tree and rerun
                    // the job with the current line height.
                    view.schedule_document_parse(cx);
                    return;
                }
                let document = view.sessions.active().editor().document();
                view.block_index
                    .publish(index, IndexSource::Formal, document);
                view.background_presentation_generation = revision.0 + 1;
                // Formal boundaries can disagree with what the bounded local
                // parse showed, so every cached presentation is re-derived once.
                view.block_cache.clear();
                view.joined_parse_cache.clear();
                let (granularity, len) = view.desired_layout();
                let snapshot_disclosure_is_current = view
                    .editor()
                    .document()
                    .revision()
                    == revision
                    && view.active_height_disclosure() == disclosure;
                if snapshot_disclosure_is_current
                    && granularity == Granularity::Blocks
                    && len == heights.len()
                {
                    view.install_heights(granularity, heights);
                    if let Some(disclosure) = disclosure {
                        view.last_background_height_disclosure = Some((revision, disclosure));
                    }
                } else {
                    // The parse was rebased onto edits, or the caret/IME moved
                    // while it ran, so the prepared heights no longer describe
                    // the current disclosure. A changed selection, including
                    // a non-empty selection collapsing to a caret, is retried
                    // in another background snapshot; rebuilding all selected
                    // blocks here would put the same document-sized walk back
                    // on the input thread at the completion boundary. The
                    // bounded active-end update keeps the caret addressable
                    // until that snapshot lands.
                    let current_disclosure = view.active_height_disclosure();
                    let selection_snapshot_requires_retry = current_disclosure
                        != disclosure
                        && (current_disclosure.is_some_and(|disclosure| !disclosure.is_empty())
                            || disclosure.is_some_and(|disclosure| !disclosure.is_empty()));
                    if selection_snapshot_requires_retry {
                        view.ensure_active_disclosure_height();
                        view.schedule_document_parse(cx);
                    } else if current_disclosure != disclosure {
                        // Moving between two caret disclosures only needs the
                        // bounded endpoint update; a whole-document snapshot
                        // would make ordinary cursor motion unnecessarily
                        // expensive.
                        view.ensure_active_disclosure_height();
                    } else {
                        view.resync_heights_for_current_disclosure();
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Coalesced per-block background job producing the whole-span parse of a
    /// joinable block that exceeds either synchronous line or byte budget — the
    /// case `presented_block` itself cannot read and reparse synchronously on
    /// every viewport miss without making a single huge paragraph's render
    /// cost scale with its length. One job per block, bounded across documents; mirrors
    /// [`Self::schedule_document_parse`]'s snapshot-and-spawn shape but at
    /// block granularity, and is what resolves a marker pair arbitrarily far
    /// apart in such a block without a fixed context window whose result
    /// would depend on where the viewport happens to sit.
    fn schedule_joined_parse(&mut self, blocks: &[IndexedBlock], cx: &mut Context<Self>) {
        let revision = self.editor().document().revision();
        for block in blocks {
            if self.joined_parse_jobs_running >= MAX_JOINED_PARSE_JOBS {
                // No backlog of obsolete viewport requests. Completion notifies
                // the view so its current visible blocks can request a free slot.
                break;
            }
            if !block_is_joinable(block.kind) {
                continue;
            }
            // Re-fetched every iteration (cheap: a reference, not a clone) so
            // its borrow never has to outlive the mutable `self` access below.
            let document = self.editor().document();
            let Some(span) = block_line_span(document, block) else {
                continue;
            };
            if block_fits_sync_join_budget(block, &span) {
                continue;
            }
            if self.joined_parse_jobs.contains_key(&block.id)
                || self
                    .joined_parse_cache
                    .get(&block.id)
                    .is_some_and(|cached| {
                        cached.revision == revision && cached.source_range == block.source_range
                    })
            {
                continue;
            }
            let content = span.start
                ..span
                    .end
                    .saturating_sub(trailing_blank_lines(document, &span));
            let snapshot = document.clone();
            let id = block.id;
            let source_range = block.source_range;
            let job = JoinedParseJob {
                document: self.document_key(),
                revision,
                source_range,
            };
            self.joined_parse_jobs.insert(id, job);
            self.joined_parse_jobs_running += 1;
            cx.spawn(async move |view, cx| {
                let parse =
                    cx.background_executor()
                        .spawn(async move {
                            parse_joined_span(&snapshot, content, source_range, revision)
                        })
                        .await;
                let _ = view.update(cx, |view, cx| {
                    // Release capacity even for an old document. Dropping a
                    // Task cannot interrupt synchronous parse already polling;
                    // capacity remains charged until it really finishes.
                    view.joined_parse_jobs_running -= 1;
                    cx.notify();
                    if view.document_key() != job.document
                        || view.joined_parse_jobs.get(&id) != Some(&job)
                    {
                        return;
                    }
                    view.joined_parse_jobs.remove(&id);
                    // Resolve against the already-published current index;
                    // never parse source synchronously to validate a result.
                    // A provisional request can retry once the formal index
                    // arrives and provides an exact block identity and range.
                    let current = view
                        .current_index()
                        .and_then(|index| index.block_at(source_range.start));
                    if view.editor().document().revision() != revision
                        || current.is_none_or(|block| {
                            block.id != id || block.source_range != source_range
                        })
                    {
                        return;
                    }
                    if let Some(parse) = parse {
                        view.joined_parse_cache.insert(
                            id,
                            JoinedBlockCache {
                                revision,
                                source_range,
                                parse,
                            },
                        );
                        // The next viewport miss on this block should read the
                        // cache instead of the presentation this view already
                        // built from a bounded, render-window-only parse.
                        view.block_cache.remove(&id);
                        cx.notify();
                    }
                });
            })
            .detach();
        }
    }

    pub(crate) fn report_error(&mut self, operation: &str, error: BufferError) {
        self.status = Some(format!("{operation} rejected: {error}"));
    }

    pub(crate) fn clear_pending_list_editing(&mut self) {
        if self.pending_list_editing.take().is_some() {
            // The transient owner is applied to the frame-local presentation,
            // not retained as document state. Drop both caches when it ends so
            // a normal frame cannot reuse its layout-only caret origin.
            self.block_cache.clear();
            self.layout_cache.clear();
        }
    }

    pub(crate) fn pending_list_indentation(&self) -> String {
        self.pending_list_editing
            .as_ref()
            .map(|context| context.indentation.clone())
            .unwrap_or_default()
    }

    /// Finds the semantic list owner of the current line before inserting a
    /// newline. Presentation supplies the owner, depth and marker alignment;
    /// this method does not inspect Markdown source to reconstruct them.
    fn list_editing_context(&self, caret_origin: ListCaretOrigin) -> Option<ListEditingContext> {
        let selection = self.editor().selection();
        if !selection.range().is_empty() {
            return None;
        }
        let caret = selection.active;
        let document = self.editor().document();
        let line = document.line_for_offset(caret).ok()?;
        let content_range = document.line_content_range(line).ok()?;
        let line_range = document.line_range(line).ok()?;
        if caret != content_range.end {
            return None;
        }
        let indexed = self.block_at_offset(caret)?;
        let revision = document.revision();
        let joined = self.joined_parse_cache.get(&indexed.id).filter(|cached| {
            cached.revision == revision && cached.source_range == indexed.source_range
        });
        let list_projection = self
            .current_index()
            .and_then(|index| index.list_projection(&indexed));
        let fence_height_projection = self
            .current_index()
            .and_then(|index| index.fence_height_projection(&indexed));
        let visible = line.0..line.0.saturating_add(1);
        let visual = presented_block_with_projections(
            self.editor(),
            &indexed,
            &visible,
            joined.map(|cached| &cached.parse),
            list_projection,
            fence_height_projection,
            self.line_height(),
        )?;
        let visual_line = visual
            .lines
            .iter()
            .find(|visual| visual.line_id == line.0 as u64)
            .filter(|visual| visual.list.is_some())?;
        let list = visual_line.list.as_ref()?;
        let indentation = list
            .source_prefix
            .and_then(|range| document.text(range).ok())
            .unwrap_or_default();
        Some(ListEditingContext {
            offset: SourceOffset(caret.0.checked_add(1)?),
            owner: list.owner.clone(),
            caret_origin,
            line_ending_len: line_range.end.0.saturating_sub(content_range.end.0),
            indentation,
        })
    }

    pub(crate) fn insert_newline(&mut self, caret_origin: ListCaretOrigin, cx: &mut Context<Self>) {
        self.clear_pending_list_editing();
        let context = self.list_editing_context(caret_origin);
        match self.editor_mut().dispatch(EditorCommand::Insert("\n")) {
            Ok(_) => {
                self.status = None;
                self.pending_list_editing = context;
            }
            Err(error) => {
                self.report_error("editor command", error);
                self.pending_list_editing = None;
            }
        }
        self.after_input(cx);
    }

    /// Inserts text after a list-aware newline. The exact source indentation
    /// before the current item's marker is deferred until the user chooses the
    /// next text, so an empty line remains an ordinary source line and a second
    /// Enter can leave the list without committing a marker or numbering.
    pub(crate) fn insert_text(&mut self, text: &str, cx: &mut Context<Self>) {
        let indentation = self.pending_list_indentation();
        self.clear_pending_list_editing();
        let mut inserted = indentation;
        inserted.push_str(text);
        match self.editor_mut().insert_text(&inserted) {
            Ok(_) => self.status = None,
            Err(error) => self.report_error("text input", error),
        }
        self.after_input(cx);
    }

    pub(crate) fn dispatch(&mut self, command: EditorCommand<'_>, cx: &mut Context<Self>) {
        self.clear_pending_list_editing();
        match self.editor_mut().dispatch(command) {
            Ok(_) => self.status = None,
            Err(error) => self.report_error("editor command", error),
        }
        self.after_input(cx);
    }

    pub(crate) fn perform_cancel_composition(&mut self, cx: &mut Context<Self>) {
        self.clear_pending_list_editing();
        if let Err(error) = self.editor_mut().cancel_composition() {
            self.report_error("composition cancel", error);
        }
        self.after_input(cx);
    }

    /// The content height scroll position is actually bounded to:
    /// `self.heights.total_height()` extended by `CARET_MODE_BADGE_HEIGHT`.
    /// Clamping to the bare content height at the document's end would put
    /// the input-mode badge drawn under the last line's caret back under the
    /// viewport's `overflow_hidden`, undoing the clearance
    /// `scroll_cursor_into_view` reserved for it (issue #240).
    fn scrollable_content_height(&self) -> f32 {
        self.heights.total_height() + CARET_MODE_BADGE_HEIGHT
    }

    /// The item (block, or physical line before a `BlockIndex` exists) and
    /// fractional position under `window_offset` (content-local window y: the
    /// mouse/gesture position's window y minus the header height), for a zoom
    /// gesture to anchor to. `None` when there is nothing laid out yet.
    fn zoom_anchor_at(&self, window_offset: f32) -> Option<PendingZoomAnchor> {
        if self.heights.is_empty() {
            return None;
        }
        let content_y = (self.scroll_y + window_offset).clamp(0.0, self.heights.total_height());
        let ordinal = self.heights.block_at_y(content_y);
        let top = self.heights.prefix_sum(ordinal);
        let height = self.heights.height(ordinal).unwrap_or(0.0);
        let fraction = if height > 0.0 {
            ((content_y - top) / height).clamp(0.0, 1.0)
        } else {
            0.0
        };
        Some(PendingZoomAnchor {
            ordinal,
            fraction,
            window_offset,
        })
    }

    /// Changes the zoom level, anchoring the document position under
    /// `window_offset` (see `zoom_anchor_at`) so it stays at the same window
    /// position once `render` installs the new zoom's heights.
    fn set_zoom_from_raw(&mut self, raw: f32, window_offset: f32, cx: &mut Context<Self>) {
        let raw = raw.clamp(MIN_ZOOM, MAX_ZOOM);
        self.raw_zoom = raw;
        let next = clamp_and_snap_zoom(raw);
        if next == self.zoom {
            return;
        }
        self.pending_zoom_anchor = self.zoom_anchor_at(window_offset);
        self.zoom = next;
        cx.notify();
    }

    fn set_zoom(&mut self, next: f32, window_offset: f32, cx: &mut Context<Self>) {
        self.set_zoom_from_raw(next, window_offset, cx);
    }

    fn apply_zoom_factor(&mut self, factor: f32, window_offset: f32, cx: &mut Context<Self>) {
        self.set_zoom_from_raw(self.raw_zoom * factor, window_offset, cx);
    }

    /// Resets zoom to 100%, anchored at the viewport's vertical center since
    /// (unlike a wheel or pinch gesture) this has no pointer position of its
    /// own.
    pub(crate) fn reset_zoom(&mut self, cx: &mut Context<Self>) {
        self.set_zoom(1.0, self.viewport_height / 2.0, cx);
    }

    /// Resolves a `PendingZoomAnchor` captured before a zoom-driven height
    /// change back to the `scroll_y` that keeps the anchored item and
    /// fraction at the same `window_offset` now that `self.heights` reflects
    /// the new zoom level. Callers must check `self.heights` is non-empty.
    fn scroll_y_for_zoom_anchor(&self, anchor: PendingZoomAnchor) -> f32 {
        let ordinal = anchor.ordinal.min(self.heights.len() - 1);
        let top = self.heights.prefix_sum(ordinal);
        let height = self.heights.height(ordinal).unwrap_or(0.0);
        let content_y = top + anchor.fraction * height;
        content_y - anchor.window_offset
    }

    fn on_scroll(&mut self, event: &ScrollWheelEvent, _: &mut Window, cx: &mut Context<Self>) {
        if event.modifiers.secondary() {
            let factor = zoom_factor_for_wheel(event.delta, self.line_height());
            let window_offset = f32::from(event.position.y) - self.theme.header_height;
            self.apply_zoom_factor(factor, window_offset, cx);
            return;
        }
        let delta = event.delta.pixel_delta(px(self.line_height()));
        self.scroll_y = clamp_scroll_y(
            self.scroll_y - f32::from(delta.y),
            self.scrollable_content_height(),
            self.viewport_height,
        );
        cx.notify();
    }

    /// macOS trackpad pinch. `event.magnification` is the incremental scale
    /// change since the previous event in the same gesture (see
    /// `gpui::MagnifyEvent`), so it is applied directly as a multiplicative
    /// factor rather than accumulated first.
    fn on_magnify(&mut self, event: &MagnifyEvent, _: &mut Window, cx: &mut Context<Self>) {
        let factor = (1.0 + event.magnification).max(0.1);
        let window_offset = f32::from(event.position.y) - self.theme.header_height;
        self.apply_zoom_factor(factor, window_offset, cx);
    }

    /// The presented line under a mouse event, from the mapping the last frame
    /// recorded. Only rendered lines can be clicked, so a miss means the frame
    /// moved under the pointer and there is nothing to do.
    #[cfg(test)]
    fn rendered_line(&self, line: usize) -> Option<VisualLine> {
        let (id, at) = self.line_owners.get(&line)?;
        self.block_cache.get(id)?.lines.get(*at).cloned()
    }

    /// The source offset a click lands on, inside one row of a presented line.
    /// Only that row's own stretch of text is measured, so a click past the end
    /// of a soft-wrapped row cannot reach text drawn on the next one.
    fn offset_at_row_x(
        &self,
        line: usize,
        fragment: Range<usize>,
        window_x: f32,
        window: &Window,
    ) -> Option<SourceOffset> {
        let (block_id, visual_line) = *self.line_owners.get(&line)?;
        let visual = self.block_cache.get(&block_id)?;
        let layout = &self.layout_cache.get(&block_id)?.layout;
        let row_index = layout
            .lines
            .iter()
            .position(|row| row.line == visual_line && row.line_visual_range == fragment)?;
        let x = window_x - self.main_column_left - self.theme.line_horizontal_padding;
        let shaper = WindowShaper::new(window, self.zoom);
        let visual_offset = layout.visual_at_x(visual, row_index, x, &shaper)?;
        let line = visual.lines.get(visual_line)?;
        let bias = collapsed_boundary_bias(line, visual_offset.0, Some(&fragment));
        layout.source_at_x_with_bias(visual, row_index, x, &shaper, bias)
    }

    fn on_editor_mouse_down(&mut self, _: &MouseDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        if !self.cancel_inline_rename(cx) {
            return;
        }
        self.blur_sidebar_filter(cx);
        self.sidebar_keyboard_focus = false;
    }

    fn on_row_mouse_down(
        &mut self,
        line: usize,
        fragment: Range<usize>,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus_handle);
        self.text_selection_drag = false;
        self.set_text_autoscroll(None, window, cx);
        let Some(offset) =
            self.offset_at_row_x(line, fragment, f32::from(event.position.x), window)
        else {
            return;
        };
        let selection = if event.modifiers.shift {
            Selection {
                anchor: self.editor().selection().anchor,
                active: offset,
            }
        } else {
            Selection::caret(offset)
        };
        self.clear_pending_list_editing();
        if let Err(error) = self.editor_mut().set_selection(selection) {
            self.report_error("mouse selection", error);
        } else {
            self.text_selection_drag = true;
            self.status = None;
            self.after_input(cx);
        }
    }

    fn on_row_mouse_move(
        &mut self,
        line: usize,
        fragment: Range<usize>,
        event: &MouseMoveEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.text_selection_drag || !event.dragging() {
            return;
        }
        let Some(offset) =
            self.offset_at_row_x(line, fragment, f32::from(event.position.x), window)
        else {
            return;
        };
        let selection = Selection {
            anchor: self.editor().selection().anchor,
            active: offset,
        };
        self.clear_pending_list_editing();
        if self.editor_mut().set_selection(selection).is_ok() {
            self.after_input(cx);
        }
    }

    /// Moves the caret one row up or down.
    ///
    /// A row is not a source line: a wrapped paragraph has several, and a
    /// heading that fits has one. The target is therefore resolved against the
    /// layout, aiming at the x of the caret when the run of vertical moves
    /// started. Without block boundaries — the first frames after a document is
    /// opened — there is no layout to resolve against, and the source-line move
    /// stands in so the caret is never stuck.
    pub(crate) fn move_vertical(
        &mut self,
        down: bool,
        extend: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.clear_pending_list_editing();
        let shaper = WindowShaper::new(window, self.zoom);
        if self.move_vertical_by_layout(down, extend, &shaper) {
            self.after_input(cx);
        } else if down {
            self.dispatch(EditorCommand::MoveDown { extend }, cx);
        } else {
            self.dispatch(EditorCommand::MoveUp { extend }, cx);
        }
    }

    /// Resolves and applies one vertical move against the layout. Returns false
    /// when the caret's block cannot be laid out, which is the caller's signal
    /// to fall back.
    fn move_vertical_by_layout(
        &mut self,
        down: bool,
        extend: bool,
        shaper: &dyn LineShaper,
    ) -> bool {
        let caret = self.editor().selection().active;
        let Some((block, layout)) = self.layout_around(caret, shaper) else {
            return false;
        };
        let Some(x) = self.editor().preferred_visual_x().or_else(|| {
            layout
                .point_for_source(&block, caret, shaper)
                .map(|point| point.x)
        }) else {
            return false;
        };
        let target = match layout.vertical_target(&block, caret, down, x, shaper) {
            VerticalMove::To(offset) => Some(offset),
            VerticalMove::PastEdge => self.neighbor_row_target(&block, down, x, shaper),
            VerticalMove::Unknown => return false,
        };
        // No neighbor means the caret is on the first or last row of the
        // document: staying put is the move.
        if let Some(target) = target
            && let Err(error) = self.editor_mut().move_vertical_to(target, extend, x)
        {
            self.report_error("vertical move", error);
        }
        true
    }

    /// The block holding `offset`, laid out, with the line above and below it so
    /// one vertical step always lands on a row that exists.
    ///
    /// The caret is nearly always inside a block the last frame drew, so this
    /// usually costs neither a parse nor a shape. When it is not — the caret was
    /// just moved off screen — only three lines of the block are presented, which
    /// matters because a block can be the whole document.
    fn layout_around(
        &self,
        offset: SourceOffset,
        shaper: &dyn LineShaper,
    ) -> Option<(VisualBlock, BlockLayout)> {
        let indexed = self.block_at_offset(offset)?;
        let line = self.editor().document().line_for_offset(offset).ok()?.0;
        let window = line.saturating_sub(1)..line + 2;
        let drawn = self
            .block_cache
            .get(&indexed.id)
            .filter(|block| block.matches(&indexed) && block.covers(&window))
            .filter(|block| self.disclosures_are_current(&indexed, block));
        if let Some(block) = drawn
            && let Some(entry) = self.layout_cache.get(&indexed.id).filter(|entry| {
                entry.is_valid(
                    self.content_width,
                    self.layout_font_revision,
                    block.revision,
                )
            })
        {
            return Some((block.clone(), entry.layout.clone()));
        }
        let revision = self.editor().document().revision();
        let joined = self.joined_parse_cache.get(&indexed.id).filter(|cached| {
            cached.revision == revision && cached.source_range == indexed.source_range
        });
        let list_projection = self
            .current_index()
            .and_then(|index| index.list_projection(&indexed));
        let fence_height_projection = self
            .current_index()
            .and_then(|index| index.fence_height_projection(&indexed));
        let visual = presented_block_with_projections(
            self.editor(),
            &indexed,
            &window,
            joined.map(|cached| &cached.parse),
            list_projection,
            fence_height_projection,
            self.line_height(),
        )?;
        let layout = layout_block(&visual, self.content_width, shaper);
        Some((visual, layout))
    }

    /// The caret target on the last row of the block above, or the first row of
    /// the block below.
    ///
    /// Resolves the neighbor block first, then looks up `joined_parse_cache`
    /// with exactly the same revision + source_range validity rule
    /// `layout_around` uses, so a joinable neighbor beyond
    /// `JOIN_SYNC_LINE_BUDGET` with a cached whole-span parse presents with the
    /// same shared-parse semantics vertical navigation would get from any other
    /// call into `presented_block` — not a navigation-only fallback that never
    /// sees a marker pair straddling the one line being targeted.
    fn neighbor_row_target(
        &self,
        block: &VisualBlock,
        down: bool,
        x: f32,
        shaper: &dyn LineShaper,
    ) -> Option<SourceOffset> {
        let (indexed, window) =
            neighbor_block_window(self.editor(), self.current_index(), block, down)?;
        let revision = self.editor().document().revision();
        let joined = self.joined_parse_cache.get(&indexed.id).filter(|cached| {
            cached.revision == revision && cached.source_range == indexed.source_range
        });
        let list_projection = self
            .current_index()
            .and_then(|index| index.list_projection(&indexed));
        target_in_neighbor(
            self.editor(),
            &indexed,
            window,
            down,
            x,
            self.content_width,
            shaper,
            joined.map(|cached| &cached.parse),
            list_projection,
            self.line_height(),
        )
    }

    /// Block boundaries around one source offset: the formal index when it
    /// describes the current revision, and a bounded local parse otherwise, the
    /// same two sources the renderer draws from.
    fn block_at_offset(&self, offset: SourceOffset) -> Option<IndexedBlock> {
        block_at_offset(self.current_index(), self.editor().document(), offset)
    }

    /// The caret rectangle the last frame drew, for the IME candidate window.
    pub(crate) fn caret_geometry(&self) -> Option<CaretGeometry> {
        self.caret_geometry
    }

    /// Scrolls so the row holding the caret is on screen.
    ///
    /// The row is the exact answer and the layout cache holds it whenever the
    /// caret's block has been drawn at the current revision, which is the case
    /// while moving around. Right after an edit the layout is a revision behind,
    /// and the caret's physical line stands in for its row — the same thing
    /// wherever nothing wraps.
    ///
    /// When the row would land flush against the viewport's bottom edge, the
    /// scroll target keeps [`CARET_MODE_BADGE_HEIGHT`] of extra clearance
    /// below it, so the input-mode badge drawn under the caret is not clipped
    /// by the viewport's `overflow_hidden` (issue #240).
    fn scroll_cursor_into_view(&mut self) {
        let editor = self.sessions.active().editor();
        let cursor = editor.selection().active;
        let Ok(line) = editor.document().line_for_offset(cursor) else {
            return;
        };
        let (top, height) = match self.granularity {
            Granularity::Lines => {
                let line_top = self.heights.prefix_sum(line.0);
                let visual_row =
                    self.line_owners
                        .get(&line.0)
                        .and_then(|(block_id, visual_line)| {
                            let layout = self
                                .layout_cache
                                .get(block_id)
                                .filter(|entry| {
                                    entry.is_valid(
                                        self.content_width,
                                        self.layout_font_revision,
                                        editor.document().revision(),
                                    )
                                })
                                .map(|entry| &entry.layout)?;
                            let row_index = layout.row_for_source(cursor)?;
                            let row = layout.lines.get(row_index)?;
                            if row.line != *visual_line {
                                return None;
                            }
                            let line_row_top = layout
                                .lines
                                .iter()
                                .find(|candidate| candidate.line == *visual_line)
                                .map(|candidate| candidate.y)?;
                            Some((line_top + row.y - line_row_top, row.height))
                        });
                visual_row.unwrap_or((line_top, self.line_height()))
            }
            Granularity::Blocks => {
                let Some(block) = self
                    .current_index()
                    .and_then(|index| index.block_at(cursor))
                else {
                    return;
                };
                let block_top = self.heights.prefix_sum(block.ordinal);
                let row = self
                    .layout_cache
                    .get(&block.id)
                    .filter(|entry| {
                        entry.is_valid(
                            self.content_width,
                            self.layout_font_revision,
                            editor.document().revision(),
                        )
                    })
                    .and_then(|entry| entry.layout.row_bounds_for_source(cursor));
                match row {
                    Some((y, height)) => (block_top + y, height),
                    None => {
                        let first = block_line_span(editor.document(), &block)
                            .map_or(line.0, |span| span.start);
                        let line_height = self.line_height();
                        let inside = line.0.saturating_sub(first) as f32 * line_height;
                        (block_top + inside, line_height)
                    }
                }
            }
        };
        self.scroll_y = scroll_y_for_cursor(
            self.scroll_y,
            top,
            height + CARET_MODE_BADGE_HEIGHT,
            self.viewport_height,
        );
    }

    /// The published index, but only while it describes the current revision.
    /// Between an edit and the incremental update that follows it, and after an
    /// edit history gap drops the index, there is none.
    fn current_index(&self) -> Option<&BlockIndex> {
        self.block_index
            .index()
            .filter(|index| index.revision() == self.editor().document().revision())
            .filter(|index| !index.is_empty())
    }

    /// What `heights` should measure, and how many entries it needs.
    fn desired_layout(&self) -> (Granularity, usize) {
        self.current_index().map_or_else(
            || (Granularity::Lines, self.editor().document().line_count()),
            |index| (Granularity::Blocks, index.len()),
        )
    }

    /// The theme's base row height scaled by the current zoom level. Every
    /// content row height and body font size (see `crate::line::block_font_size`)
    /// derives from this or from `zoom` directly, so zooming moves them together.
    /// Sidebar and header sizing intentionally do not use this.
    pub(crate) fn line_height(&self) -> f32 {
        self.theme.line_height * self.zoom
    }

    /// Initial height of every entry, from the line height alone. Measured
    /// heights replace these as blocks are drawn; R4C keeps them across rebuilds.
    fn item_heights(&self) -> Vec<f32> {
        let line_height = self.line_height();
        let (granularity, len) = self.desired_layout();
        match granularity {
            Granularity::Lines => vec![line_height; len],
            Granularity::Blocks => {
                let document = self.editor().document();
                let index = self
                    .current_index()
                    .expect("block granularity has an index");
                let disclosure = self.active_height_disclosure();
                block_heights_with_disclosure(document, index, line_height, disclosure)
            }
        }
    }

    /// Keeps the active endpoint's fence block in the height index before the
    /// caret-scroll calculation runs. A caret move does not change the block
    /// count, so `resync_heights` quite deliberately keeps its measured index;
    /// that fast path must still expand a zero-height block that was just made
    /// editable or it can disappear from the next virtualization window.
    ///
    /// A non-empty selection is different: its complete disclosure is rebuilt
    /// by the coalesced background parse. Updating every block it spans here
    /// would make shift-selection and mouse dragging proportional to document
    /// size. Only the two selection endpoints and the IME range boundaries are
    /// needed synchronously to keep the caret addressable while that snapshot
    /// is pending.
    fn ensure_active_disclosure_height(&mut self) {
        if self.granularity != Granularity::Blocks {
            return;
        }
        let Some(disclosure) = self.active_height_disclosure() else {
            return;
        };
        let Some(index) = self.current_index() else {
            return;
        };
        let line_height = self.line_height();
        let revision = self.editor().document().revision();
        let previous = self
            .last_applied_height_disclosure
            .filter(|(previous_revision, _)| *previous_revision == revision)
            .map(|(_, disclosure)| disclosure);
        let mut ordinals = Vec::with_capacity(4);
        let mut add_ordinal = |offset: SourceOffset| {
            if let Some(ordinal) = index.ordinal_at(offset)
                && !ordinals.contains(&ordinal)
            {
                ordinals.push(ordinal);
            }
        };
        let selection = self.editor().selection();
        add_ordinal(selection.anchor);
        add_ordinal(selection.active);
        if let Some(previous) = previous {
            add_ordinal(previous.start);
            if !previous.is_empty() {
                add_ordinal(SourceOffset(previous.end.0.saturating_sub(1)));
            }
        }
        if let Some(ime) = self.editor().ime() {
            add_ordinal(ime.current_range.start);
            if !ime.current_range.is_empty() {
                add_ordinal(SourceOffset(ime.current_range.end.0.saturating_sub(1)));
            }
        }
        let updates = ordinals
            .into_iter()
            .filter_map(|ordinal| {
                let block = index.block(ordinal)?;
                let projection = index.fence_height_projection(&block)?;
                let collapsed = projection.inactive_rows_in(
                    block.source_range,
                    block.source_range,
                    Some(disclosure),
                );
                let minimum = line_height * block.line_count.saturating_sub(collapsed) as f32;
                let current = self.heights.height(ordinal)?;
                let target = previous.map_or_else(
                    || current.max(minimum),
                    |previous| {
                        let previous_collapsed = projection.inactive_rows_in(
                            block.source_range,
                            block.source_range,
                            Some(previous),
                        );
                        // `current` may be a measured layout height: a code
                        // row is taller than the plain line-height seed. Move
                        // only the fence-row delta between disclosures so the
                        // measured body rows stay measured instead of being
                        // replaced by an arithmetic lower bound.
                        let collapsed_delta = collapsed as f32 - previous_collapsed as f32;
                        (current - code_line_height(line_height) * collapsed_delta).max(minimum)
                    },
                );
                (target != current).then_some((ordinal, target))
            })
            .collect::<Vec<_>>();
        for (ordinal, minimum) in updates {
            self.heights.update(ordinal, minimum);
        }
        self.last_applied_height_disclosure = Some((revision, disclosure));
    }

    fn active_height_disclosure(&self) -> Option<SourceRange> {
        self.editor()
            .ime()
            .map(|ime| ime.current_range)
            .or_else(|| Some(self.editor().selection().range()))
    }

    /// Keeps `heights` keyed to the same thing the renderer enumerates. This is
    /// the full synchronization path used for startup and index publication;
    /// input uses `apply_height_index_update` to touch only its parse window.
    fn resync_heights(&mut self) {
        let (granularity, len) = self.desired_layout();
        if granularity == self.granularity && len == self.heights.len() {
            return;
        }
        let heights = HeightIndex::new(self.item_heights());
        self.install_heights(granularity, heights);
    }

    /// Rebuilds the height snapshot even when its shape did not change. A
    /// formal parse can finish after the caret or IME moved without changing
    /// the block count; retaining the old same-sized tree would retain the
    /// previous fence disclosure and make presentation and virtualization
    /// disagree.
    fn resync_heights_for_current_disclosure(&mut self) {
        let (granularity, len) = self.desired_layout();
        if granularity == self.granularity && len == self.heights.len() {
            self.install_heights(granularity, HeightIndex::new(self.item_heights()));
        } else {
            self.resync_heights();
        }
    }

    /// Swaps in a height index, keeping the reader where they were: the scroll
    /// position is carried across on the source offset at the top of the
    /// viewport, read before the swap and resolved after it.
    fn install_heights(&mut self, granularity: Granularity, heights: HeightIndex) {
        let anchor = self.top_source_offset();
        let intra = (!self.heights.is_empty()).then(|| {
            let item = self.heights.block_at_y(self.scroll_y);
            self.scroll_y - self.heights.prefix_sum(item)
        });
        self.granularity = granularity;
        self.heights = heights;
        self.height_blocks = if granularity == Granularity::Blocks {
            self.current_index()
                .map(|index| {
                    index
                        .blocks()
                        .map(|block| HeightBlock {
                            id: block.id,
                            line_count: block.line_count,
                        })
                        .collect()
                })
                .unwrap_or_default()
        } else {
            HeightBlocks::default()
        };
        self.scroll_y = anchor.map_or(self.scroll_y, |offset| {
            let top = self.scroll_for_offset(offset);
            let item = self.heights.block_at_y(top);
            let inside = intra
                .zip(self.heights.height(item))
                .map_or(0.0, |(intra, height)| intra.clamp(0.0, height));
            top + inside
        });
        self.last_applied_height_disclosure = self
            .active_height_disclosure()
            .map(|disclosure| (self.editor().document().revision(), disclosure));
    }

    /// Applies the small splice reported by the incremental block index. The
    /// common case (typing without changing block boundaries or line count)
    /// compares a handful of entries and leaves the height tree untouched.
    fn apply_height_index_update(&mut self, update: &BlockIndexUpdate) -> bool {
        if self.granularity != Granularity::Blocks {
            return false;
        }
        let first = update.first_replaced_block;
        let old_end = first.saturating_add(update.replaced_blocks);
        if old_end > self.height_blocks.len() {
            return false;
        }
        let Some(index) = self.current_index() else {
            return false;
        };
        if index.len() != self.height_blocks.len() - update.replaced_blocks + update.inserted_blocks
        {
            return false;
        }
        let next = (first..first + update.inserted_blocks)
            .filter_map(|ordinal| index.block(ordinal))
            .map(|block| HeightBlock {
                id: block.id,
                line_count: block.line_count,
            })
            .collect::<Vec<_>>();
        if next.len() != update.inserted_blocks {
            return false;
        }
        if self.height_blocks.range_eq(first..old_end, &next) {
            return true;
        }
        let old_top = (!self.heights.is_empty()).then(|| {
            let ordinal = self.heights.block_at_y(self.scroll_y);
            let id = self.height_blocks.get(ordinal).map(|block| block.id);
            let intra = self.scroll_y - self.heights.prefix_sum(ordinal);
            (id, ordinal, intra)
        });
        let line_height = self.line_height();
        self.heights.splice(
            first..old_end,
            next.iter()
                .map(|block| line_height * block.line_count as f32),
        );
        self.height_blocks.splice(first..old_end, &next);

        if let Some((id, old_ordinal, intra)) = old_top {
            let ordinal = rebase_ordinal_after_splice(
                old_ordinal,
                id,
                first..old_end,
                &next,
                self.heights.len(),
            );
            let inside = self
                .heights
                .height(ordinal)
                .map_or(0.0, |height| intra.clamp(0.0, height));
            self.scroll_y = self.heights.prefix_sum(ordinal) + inside;
        }
        true
    }

    /// Source offset of the item currently at the top of the viewport.
    fn top_source_offset(&self) -> Option<SourceOffset> {
        if self.heights.is_empty() {
            return None;
        }
        let item = self.heights.block_at_y(self.scroll_y);
        match self.granularity {
            Granularity::Lines => self
                .editor()
                .document()
                .line_range(LineId(item))
                .ok()
                .map(|range| range.start),
            Granularity::Blocks => self
                .current_index()
                .and_then(|index| index.block(item))
                .map(|block| block.source_range.start),
        }
    }

    fn scroll_for_offset(&self, offset: SourceOffset) -> f32 {
        match self.granularity {
            Granularity::Lines => self
                .editor()
                .document()
                .line_for_offset(offset)
                .map(|line| line.0)
                .ok()
                .map_or(self.scroll_y, |line| self.heights.prefix_sum(line)),
            Granularity::Blocks => {
                let document = self.editor().document();
                let Some(block) = self
                    .current_index()
                    .and_then(|index| index.block_at(offset))
                else {
                    return self.scroll_y;
                };
                let first_line = block_line_span(document, &block).map_or(0, |span| span.start);
                let line = document
                    .line_for_offset(offset)
                    .map_or(first_line, |line| line.0);
                let physical_line = line.saturating_sub(first_line);
                let visible_line = self.visible_line_prefix(&block, physical_line);
                self.heights.prefix_sum(block.ordinal)
                    + visible_line as f32 * self.drawn_line_height(&block)
            }
        }
    }

    /// Number of non-collapsed physical lines before `physical_line` inside a
    /// block. Fence rows are answered by the formal projection's prefix index;
    /// the render path never enumerates the block's off-screen fences.
    fn visible_line_prefix(&self, block: &IndexedBlock, physical_line: usize) -> usize {
        let Some(projection) = self
            .current_index()
            .and_then(|index| index.fence_height_projection(block))
        else {
            return physical_line;
        };
        let collapsed = projection.inactive_rows_before_line(
            block.source_range,
            physical_line,
            self.active_height_disclosure(),
        );
        physical_line.saturating_sub(collapsed)
    }

    /// Inverts [`Self::visible_line_prefix`] for a visual row count. The
    /// monotone prefix query is inverted with a binary search, so a large
    /// list/quote block remains bounded by the logarithm of its physical line
    /// count even when it contains many collapsed fence rows.
    fn physical_line_prefix_for_visible_rows(
        &self,
        block: &IndexedBlock,
        physical_line_count: usize,
        visible_rows: usize,
    ) -> usize {
        invert_visible_line_prefix(physical_line_count, visible_rows, |line| {
            self.visible_line_prefix(block, line)
        })
    }

    /// The blocks covering the viewport. `visible` is a range of height entries,
    /// which at block granularity are ordinals and until an index exists are
    /// physical lines.
    fn visible_blocks(&self, visible: Range<usize>) -> Vec<IndexedBlock> {
        match self.granularity {
            Granularity::Blocks => {
                let Some(index) = self.current_index() else {
                    return Vec::new();
                };
                visible.filter_map(|ordinal| index.block(ordinal)).collect()
            }
            Granularity::Lines => {
                let document = self.sessions.active().editor().document();
                let last_line = document.line_count().saturating_sub(1);
                let first = visible.start.min(last_line);
                let last = visible.end.saturating_sub(1).min(last_line);
                let (Ok(first_range), Ok(last_range)) = (
                    document.line_range(LineId(first)),
                    document.line_range(LineId(last)),
                ) else {
                    return Vec::new();
                };
                // One bounded parse of the viewport neighborhood, never the
                // document: this runs on the render path.
                let local = local_block_index(document, first..last + 1);
                let span = SourceRange::new(
                    first_range.start.0,
                    last_range.end.0.max(first_range.start.0),
                );
                let blocks = local.blocks_in(span).collect::<Vec<_>>();
                if blocks.is_empty() {
                    // A document with no Markdown block at all still has a line
                    // to draw the caret on.
                    let empty = IndexedBlock::provisional_paragraph(
                        document.revision(),
                        SourceRange::new(first_range.start.0, last_range.end.0),
                        last - first + 1,
                    );
                    return vec![empty];
                }
                blocks
            }
        }
    }

    /// Document lines the viewport shows, which is what each block presents the
    /// intersection with.
    ///
    /// At line granularity the visible range already is that window. At block
    /// granularity it has to be read back out of the scroll geometry, because a
    /// block can be taller than the viewport — a document with no blank line in
    /// it is one block. How tall a line is depends on how often it wraps, so the
    /// block's own laid-out rows answer it where they exist and the line height
    /// stands in where they do not; the overscan absorbs the difference. When a
    /// fence projection collapses rows, the visual estimate is inverted through
    /// its prefix count before the physical source window is selected.
    fn visible_line_window(&self, blocks: &[IndexedBlock], items: &Range<usize>) -> Range<usize> {
        match self.granularity {
            Granularity::Lines => items.clone(),
            Granularity::Blocks => {
                let document = self.sessions.active().editor().document();
                let top = (self.scroll_y - self.theme.overscan).max(0.0);
                let bottom = self.scroll_y + self.viewport_height + self.theme.overscan;
                let (Some(first), Some(last)) = (blocks.first(), blocks.last()) else {
                    return 0..0;
                };
                let (Some(first_span), Some(last_span)) = (
                    block_line_span(document, first),
                    block_line_span(document, last),
                ) else {
                    return 0..0;
                };
                let local_rows = |y: f32, block: &IndexedBlock, round_up: bool| {
                    let rows = ((y - self.heights.prefix_sum(block.ordinal)).max(0.0)
                        / self.drawn_line_height(block))
                        .max(0.0);
                    if round_up {
                        rows.ceil() as usize + 1
                    } else {
                        rows.floor() as usize + 1
                    }
                };
                let start_prefix = self.physical_line_prefix_for_visible_rows(
                    first,
                    first_span.len(),
                    local_rows(top, first, false),
                );
                let end_prefix = self.physical_line_prefix_for_visible_rows(
                    last,
                    last_span.len(),
                    local_rows(bottom, last, true),
                );
                let start = first_span.start + start_prefix.saturating_sub(1);
                let end = last_span.start + end_prefix;
                let start = start.min(first_span.end.saturating_sub(1));
                start..end.max(start + 1).min(last_span.end)
            }
        }
    }

    /// How tall one line of a block draws, on average. A wrapped line occupies
    /// several rows, so the plain line height under-counts it; the block's own
    /// layout says by how much once it has been drawn once.
    fn drawn_line_height(&self, block: &IndexedBlock) -> f32 {
        self.layout_cache
            .get(&block.id)
            .filter(|entry| {
                entry.layout.width == self.content_width
                    && entry.font_revision == self.layout_font_revision
            })
            .and_then(|entry| entry.layout.average_line_height())
            .unwrap_or(self.line_height())
            .max(1.0)
    }

    /// Presents the part of a block that reaches `visible`, reusing the cached
    /// presentation when the document has not touched it, the index still
    /// describes it the same way, and it already covers the wanted lines.
    ///
    /// Reports whether the presentation was reused, because the rows laid out
    /// from it can be reused exactly when it was.
    fn cached_block(
        &mut self,
        block: &IndexedBlock,
        visible: &Range<usize>,
    ) -> Option<(VisualBlock, bool)> {
        let revision = self.editor().document().revision();
        if let Some(mut cached) = self.block_cache.remove(&block.id) {
            let editor = self.sessions.active().editor();
            let reusable = if cached.revision == revision {
                true
            } else if let Ok(deltas) = editor.document().deltas_since(cached.revision) {
                !deltas
                    .iter()
                    .any(|delta| edit_affects_range(*delta, cached.source_range))
                    && cached.rebase(&deltas, revision)
                    // The rows describe the same text at shifted offsets, so
                    // they move with the block rather than being rebuilt.
                    && self
                        .layout_cache
                        .get_mut(&block.id)
                        .is_none_or(|entry| entry.layout.rebase(&deltas, revision))
            } else {
                false
            };
            if reusable
                && cached.matches(block)
                && cached.covers(visible)
                && self.disclosures_are_current(block, &cached)
            {
                self.block_cache.insert(block.id, cached.clone());
                if let Some(context) = &self.pending_list_editing
                    && apply_list_editing_context(&mut cached, context)
                {
                    return Some((cached, false));
                }
                return Some((cached, true));
            }
        }
        let joined = self.joined_parse_cache.get(&block.id).filter(|cached| {
            cached.revision == revision && cached.source_range == block.source_range
        });
        let list_projection = self
            .current_index()
            .and_then(|index| index.list_projection(block));
        let fence_height_projection = self
            .current_index()
            .and_then(|index| index.fence_height_projection(block));
        let mut presented = presented_block_with_projections(
            self.sessions.active().editor(),
            block,
            visible,
            joined.map(|cached| &cached.parse),
            list_projection,
            fence_height_projection,
            self.line_height(),
        )?;
        self.block_cache.insert(block.id, presented.clone());
        if let Some(context) = &self.pending_list_editing
            && apply_list_editing_context(&mut presented, context)
        {
            return Some((presented, false));
        }
        Some((presented, false))
    }

    /// Rows for a presented block, shaped only when the presentation is new or
    /// the text column changed width.
    fn block_layout(
        &mut self,
        block: &VisualBlock,
        reused: bool,
        shaper: &dyn LineShaper,
    ) -> BlockLayout {
        if reused
            && let Some(cached) = self.layout_cache.get(&block.id)
            && cached.is_valid(
                self.content_width,
                self.layout_font_revision,
                block.revision,
            )
        {
            #[cfg(any(feature = "instrument", feature = "timing-probe"))]
            {
                self.instrumentation.layout_cache_hits += 1;
            }
            return cached.layout.clone();
        }
        #[cfg(any(feature = "instrument", feature = "timing-probe"))]
        {
            self.instrumentation.layout_cache_misses += 1;
        }
        let layout = layout_block(block, self.content_width, shaper);
        self.layout_cache.insert(
            block.id,
            LayoutCacheEntry {
                layout: layout.clone(),
                font_revision: self.layout_font_revision,
            },
        );
        layout
    }

    /// True while every line of a cached block still discloses what the caret,
    /// selection and IME say it should.
    ///
    /// Compares against [`expected_block_disclosures`] rather than each line's
    /// own disclosure in isolation, because a shared construct joined across
    /// physical lines discloses with one merged, run-wide value (see
    /// `hane_presentation::merged_disclosure`) — a per-line-only recomputation
    /// would disagree with that value on every line but the one the caret,
    /// selection or IME actually sits on, and invalidate the cache every frame
    /// the caret sits inside an active multi-line paragraph.
    fn disclosures_are_current(&self, indexed: &IndexedBlock, block: &VisualBlock) -> bool {
        let revision = self.editor().document().revision();
        let render = block.span.start + block.lines_before
            ..block.span.start + block.lines_before + block.lines.len();
        let joined = self.joined_parse_cache.get(&indexed.id).filter(|cached| {
            cached.revision == revision && cached.source_range == indexed.source_range
        });
        let Some(expected) = expected_block_disclosures(
            self.editor(),
            indexed,
            &render,
            joined.map(|cached| &cached.parse),
        ) else {
            return false;
        };
        expected.len() == block.lines.len()
            && expected
                .iter()
                .zip(&block.lines)
                .all(|((line, disclosure), visual)| {
                    *line as u64 == visual.line_id && *disclosure == visual.disclosure
                })
    }
}

/// The adjacent `IndexedBlock` a vertical step off the edge of `block` would
/// land in, and the one-line window on it the caret can land on — the "which
/// block, which line" half of [`neighbor_row_target`], kept separate from
/// presenting and laying it out so a caller (see
/// `EditorView::neighbor_row_target`) can look up a cached [`JoinedParse`] for
/// the resolved block's own id before presenting it, instead of the resolve
/// and the presentation being fused into one call that never gets a chance to
/// supply one.
fn neighbor_block_window(
    editor: &Editor,
    index: Option<&BlockIndex>,
    block: &VisualBlock,
    down: bool,
) -> Option<(IndexedBlock, Range<usize>)> {
    let document = editor.document();
    let probe = if down {
        let next = block.source_range.end;
        (next.0 < document.len_bytes().0).then_some(next)?
    } else {
        SourceOffset(block.source_range.start.0.checked_sub(1)?)
    };
    let indexed = block_at_offset(index, document, probe)?;
    let span = block_line_span(document, &indexed)?;
    let window = if down {
        span.start..span.start + 1
    } else {
        span.end.saturating_sub(1)..span.end
    };
    Some((indexed, window))
}

/// Presents and lays out an already-resolved neighbor block (see
/// [`neighbor_block_window`]) and answers the caret target on the row the
/// caller is stepping onto. `joined`, when the caller has a valid cached
/// whole-span parse for `indexed`, is threaded straight through to
/// `presented_block` so a joinable neighbor beyond `JOIN_SYNC_LINE_BUDGET`
/// resolves the same marker pairs vertical navigation's one-line window would
/// otherwise never see the other half of.
#[allow(clippy::too_many_arguments)]
fn target_in_neighbor(
    editor: &Editor,
    indexed: &IndexedBlock,
    window: Range<usize>,
    down: bool,
    x: f32,
    width: f32,
    shaper: &dyn LineShaper,
    joined: Option<&JoinedParse>,
    list_projection: Option<&ListProjection>,
    line_height: f32,
) -> Option<SourceOffset> {
    let visual = presented_block_with_list_projection(
        editor,
        indexed,
        &window,
        joined,
        list_projection,
        line_height,
    )?;
    let layout = layout_block(&visual, width, shaper);
    let row = if down {
        0
    } else {
        layout.lines.len().checked_sub(1)?
    };
    layout.source_at_x(&visual, row, x, shaper)
}

/// The caret target one row into the next or previous block, aiming at `x`.
///
/// Only the one line of the neighbor that the caret can land on is presented, so
/// stepping off the edge of a block costs the same whatever the neighbor's size,
/// unless `joined` supplies a cached whole-span parse for it. Test-only: the
/// real render path is `EditorView::neighbor_row_target`, which resolves the
/// neighbor itself (see [`neighbor_block_window`]) so it can look up a cached
/// [`JoinedParse`] by the resolved block's own id before presenting it (see
/// [`target_in_neighbor`]) — this is the resolve-then-present pair fused back
/// together for tests that supply `joined` directly instead of through a
/// cache.
#[cfg(test)]
#[allow(clippy::too_many_arguments)]
fn neighbor_row_target(
    editor: &Editor,
    index: Option<&BlockIndex>,
    block: &VisualBlock,
    down: bool,
    x: f32,
    width: f32,
    shaper: &dyn LineShaper,
    joined: Option<&JoinedParse>,
) -> Option<SourceOffset> {
    let (indexed, window) = neighbor_block_window(editor, index, block, down)?;
    target_in_neighbor(
        editor,
        &indexed,
        window,
        down,
        x,
        width,
        shaper,
        joined,
        None,
        DEFAULT_LINE_HEIGHT,
    )
}

/// The block holding one source offset: the formal index while it describes the
/// current revision, and a bounded local parse otherwise — the same two sources
/// the renderer draws from.
fn block_at_offset(
    index: Option<&BlockIndex>,
    document: &RopeBuffer,
    offset: SourceOffset,
) -> Option<IndexedBlock> {
    if let Some(index) = index {
        return index.block_at(offset);
    }
    let line = document.line_for_offset(offset).ok()?.0;
    local_block_index(document, line..line + 2).block_at(offset)
}

/// Resolves a physical source line to the block that owns it. Every byte belongs
/// to exactly one block, so the line's start offset is enough; this is the seam
/// the block-based renderer will pull on in R4A.
fn block_at_line(index: &BlockIndex, document: &RopeBuffer, line: usize) -> Option<IndexedBlock> {
    let range = document.line_range(LineId(line)).ok()?;
    index.block_at(range.start)
}

fn edit_affects_range(delta: RevisionDelta, range: SourceRange) -> bool {
    let edit = delta.edited_source_range_before;
    if edit.is_empty() {
        range.start <= edit.start && edit.start <= range.end
    } else {
        range.intersects(edit)
    }
}

impl Focusable for EditorView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

#[cfg(any(feature = "instrument", feature = "timing-probe"))]
impl EditorView {
    pub fn arm_startup_timing(&mut self, process_started: Instant) {
        self.instrumentation.process_started = process_started;
        self.instrumentation.ready_armed = true;
    }

    #[cfg(feature = "instrument")]
    pub fn record_phase0_idle_memory(&mut self, rss_bytes: Option<u64>) {
        if let Some(output) = &mut self.instrumentation.metrics_output
            && let Err(error) = output.memory("memory_idle_30s", rss_bytes)
        {
            eprintln!("could not write idle memory metrics: {error}");
        }
    }

    #[cfg(feature = "instrument")]
    pub fn apply_phase0_background_presentation(
        &mut self,
        generation: u64,
        cx: &mut Context<Self>,
    ) {
        self.background_presentation_generation = generation;
        cx.notify();
    }

    #[cfg(feature = "instrument")]
    pub fn enable_display_linked_scroll_measurement(&mut self) {
        self.instrumentation.display_linked_scroll_direction = Some(1.0);
    }

    #[cfg(feature = "instrument")]
    fn apply_phase1_scroll_frame(&mut self) {
        let direction = self
            .instrumentation
            .display_linked_scroll_direction
            .unwrap_or(1.0);
        let max = (self.heights.total_height() - self.viewport_height).max(0.0);
        let proposed = self.scroll_y + direction * 72.0;
        let next_direction = if proposed <= 0.0 {
            1.0
        } else if proposed >= max {
            -1.0
        } else {
            direction
        };
        self.scroll_y = proposed.clamp(0.0, max);
        self.instrumentation.display_linked_scroll_direction = Some(next_direction);
    }

    pub(crate) fn step_measurement_scroll(&mut self, window: &mut Window) {
        #[cfg(feature = "instrument")]
        if self
            .instrumentation
            .display_linked_scroll_direction
            .is_some()
        {
            self.apply_phase1_scroll_frame();
            window.request_animation_frame();
        }
        #[cfg(not(feature = "instrument"))]
        let _ = window;
    }

    #[cfg(feature = "instrument")]
    /// Moves the cursor for an instrumentation run.
    ///
    /// # Errors
    ///
    /// Returns [`BufferError`] when `offset` is not a valid source boundary.
    pub fn set_cursor_offset_for_measurement(
        &mut self,
        offset: usize,
        cx: &mut Context<Self>,
    ) -> Result<(), BufferError> {
        self.editor_mut()
            .set_selection(hane_editor::Selection::caret(hane_document::SourceOffset(
                offset,
            )))?;
        self.after_input(cx);
        Ok(())
    }

    #[cfg(feature = "instrument")]
    /// Moves the cursor down for an instrumentation run.
    ///
    /// # Errors
    ///
    /// Returns [`BufferError`] when an editor command cannot be applied.
    pub fn move_cursor_down_for_development(
        &mut self,
        count: usize,
        cx: &mut Context<Self>,
    ) -> Result<(), BufferError> {
        for _ in 0..count {
            self.editor_mut()
                .dispatch(EditorCommand::MoveDown { extend: false })?;
        }
        self.after_input(cx);
        Ok(())
    }

    fn record_block_index_update(&mut self, update: &BlockIndexUpdate) {
        if let Some(output) = &mut self.instrumentation.metrics_output
            && let Err(error) = output.block_index(update)
        {
            eprintln!("could not write block index metrics: {error}");
        }
    }

    pub(crate) fn record_frame_instrumentation(
        &mut self,
        measurements: &[InputMeasurement],
        interval: Option<Duration>,
        layout: Option<Duration>,
    ) {
        let instrumentation = &mut self.instrumentation;
        if instrumentation.ready_armed && !instrumentation.ready_reported {
            instrumentation.ready_reported = true;
            let startup = instrumentation.process_started.elapsed();
            let rss = hane_metrics::process_memory_bytes();
            eprintln!(
                "hane_ready startup_time_ms={:.3} file_open_time_ms={:.3} rss_bytes={}",
                startup.as_secs_f64() * 1_000.0,
                instrumentation.file_open_time.as_secs_f64() * 1_000.0,
                rss.unwrap_or(0),
            );
            if let Some(output) = &mut instrumentation.metrics_output {
                if let Err(error) = output.memory("memory_load", instrumentation.load_rss_bytes) {
                    eprintln!("could not write load memory metrics: {error}");
                }
                if let Err(error) = output.ready(startup, instrumentation.file_open_time, rss) {
                    eprintln!("could not write ready metrics: {error}");
                }
            }
        }
        if let Some(output) = &mut instrumentation.metrics_output {
            let cache = (
                instrumentation.layout_cache_hits,
                instrumentation.layout_cache_misses,
            );
            if let Err(error) = output.paint(interval, layout, cache) {
                eprintln!("could not write paint metrics: {error}");
            }
            for measurement in measurements {
                if let Err(error) = output.input(measurement) {
                    eprintln!("could not write input metrics: {error}");
                }
            }
        }
        instrumentation.layout_cache_hits = 0;
        instrumentation.layout_cache_misses = 0;
        if !measurements.is_empty() {
            log_summary(&self.metrics);
        }
    }
}

fn invert_visible_line_prefix(
    physical_line_count: usize,
    visible_rows: usize,
    mut visible_prefix: impl FnMut(usize) -> usize,
) -> usize {
    let visible_rows = visible_rows.min(visible_prefix(physical_line_count));
    if visible_rows == 0 {
        return 0;
    }
    let mut low = 0;
    let mut high = physical_line_count;
    while low < high {
        let middle = low + (high - low) / 2;
        if visible_prefix(middle) >= visible_rows {
            high = middle;
        } else {
            low = middle + 1;
        }
    }
    low
}

/// The header's status line, combining a persistent `draft_recovery_warning`
/// with whatever transient `status` is current. Neither may swallow the
/// other: the warning has to survive `open_path` cycling `status` through
/// "Opening…"/"Opened" right after the scan that raised it, but a later
/// status the user needs for data safety (a save failure, a conflict) must
/// stay visible too, since this view only re-scans a work folder once at
/// startup and an unconditional priority for the warning would otherwise
/// hide every status for the rest of the session. `None` when neither is
/// set, so the caller can fall back to its own default line.
fn header_status_line(warning: Option<&str>, status: Option<&str>) -> Option<String> {
    match (warning, status) {
        (Some(warning), Some(status)) => Some(format!("{warning} · {status}")),
        (Some(warning), None) => Some(warning.to_owned()),
        (None, Some(status)) => Some(status.to_owned()),
        (None, None) => None,
    }
}

#[cfg(not(any(feature = "instrument", feature = "timing-probe")))]
impl EditorView {
    pub(crate) fn step_measurement_scroll(&mut self, _window: &mut Window) {}

    fn record_block_index_update(&mut self, _update: &BlockIndexUpdate) {}

    pub(crate) fn record_frame_instrumentation(
        &mut self,
        _measurements: &[InputMeasurement],
        _interval: Option<Duration>,
        _layout: Option<Duration>,
    ) {
    }
}

impl EditorView {
    fn begin_sidebar_resize(
        &mut self,
        event: &MouseDownEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.sidebar_resize_drag = Some(SidebarResizeDrag {
            pointer_x: f32::from(event.position.x),
            sidebar_width: self.sidebar_width,
        });
        cx.stop_propagation();
        cx.notify();
    }

    fn begin_editor_scrollbar_drag(
        &mut self,
        event: &MouseDownEvent,
        content_height: f32,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.editor_scrollbar_drag = Some(ScrollbarDrag {
            pointer_y: f32::from(event.position.y),
            scroll_y: self.scroll_y,
            viewport_height: self.viewport_height,
            content_height,
        });
        cx.stop_propagation();
        cx.notify();
    }

    fn begin_sidebar_scrollbar_drag(
        &mut self,
        event: &MouseDownEvent,
        viewport_height: f32,
        content_height: f32,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let current_scroll = (-f32::from(self.sidebar_scroll.offset().y))
            .clamp(0.0, (content_height - viewport_height).max(0.0));
        self.sidebar_scrollbar_drag = Some(ScrollbarDrag {
            pointer_y: f32::from(event.position.y),
            scroll_y: current_scroll,
            viewport_height,
            content_height,
        });
        cx.stop_propagation();
        cx.notify();
    }

    fn on_panel_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut changed = false;
        if let Some(drag) = self.sidebar_resize_drag {
            let delta = f32::from(event.position.x) - drag.pointer_x;
            let next = sidebar_width_for_drag(
                drag.sidebar_width,
                delta,
                f32::from(window.viewport_size().width),
            );
            if next != self.sidebar_width {
                self.sidebar_width = next;
                changed = true;
            }
        }
        if let Some(drag) = self.editor_scrollbar_drag {
            let delta = f32::from(event.position.y) - drag.pointer_y;
            let next = scroll_y_for_thumb_drag(
                drag.scroll_y,
                delta,
                drag.viewport_height,
                drag.content_height,
            );
            if next != self.scroll_y {
                self.scroll_y = next;
                changed = true;
            }
        }
        if let Some(drag) = self.sidebar_scrollbar_drag {
            let delta = f32::from(event.position.y) - drag.pointer_y;
            let next = scroll_y_for_thumb_drag(
                drag.scroll_y,
                delta,
                drag.viewport_height,
                drag.content_height,
            );
            self.sidebar_scroll.set_offset(point(px(0.0), px(-next)));
            changed = true;
        }
        // Sidebar resize / scrollbar drags above already claimed this move;
        // a text-selection drag never sets those, so this only fires for the
        // drag `on_row_mouse_down` started (issue #213).
        if self.text_selection_drag
            && event.dragging()
            && self.sidebar_resize_drag.is_none()
            && self.sidebar_scrollbar_drag.is_none()
            && self.editor_scrollbar_drag.is_none()
        {
            let direction = self.text_autoscroll_direction_for(f32::from(event.position.y));
            self.set_text_autoscroll(direction, window, cx);
        }
        if changed {
            cx.stop_propagation();
            cx.notify();
        }
    }

    /// Which edge of the editor viewport `window_y` (window-space, matching
    /// `MouseMoveEvent::position`) sits inside, if any. The viewport begins
    /// right below the header and is `self.viewport_height` tall, the same
    /// frame `render` computes it in.
    fn text_autoscroll_direction_for(&self, window_y: f32) -> Option<AutoscrollDirection> {
        let local_y = window_y - self.theme.header_height;
        let near_top = local_y < TEXT_SELECTION_AUTOSCROLL_EDGE;
        let near_bottom = local_y > self.viewport_height - TEXT_SELECTION_AUTOSCROLL_EDGE;
        match (near_top, near_bottom) {
            (true, true) => {
                // On a very short viewport the two fixed-size edge zones
                // overlap. Pick the nearer edge so a pointer near the bottom
                // cannot be misread as an upward drag merely because the top
                // check happens to run first.
                if local_y < self.viewport_height / 2.0 {
                    Some(AutoscrollDirection::Up)
                } else {
                    Some(AutoscrollDirection::Down)
                }
            }
            (true, false) => Some(AutoscrollDirection::Up),
            (false, true) => Some(AutoscrollDirection::Down),
            (false, false) => None,
        }
    }

    /// Starts, stops, or redirects the text-selection autoscroll loop to
    /// match `direction`, doing nothing when it already matches what is
    /// currently running.
    fn set_text_autoscroll(
        &mut self,
        direction: Option<AutoscrollDirection>,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        if self.text_autoscroll == direction {
            return;
        }
        self.text_autoscroll = direction;
        self.text_autoscroll_activity = self.text_autoscroll_activity.wrapping_add(1);
        if let Some(direction) = direction {
            self.spawn_text_autoscroll(direction, self.text_autoscroll_activity, window, cx);
        }
    }

    /// Repeatedly extends the selection by one row toward `direction` and
    /// lets `after_input`'s `scroll_cursor_into_view` follow it, every
    /// `TEXT_SELECTION_AUTOSCROLL_INTERVAL`, so a held pointer near an edge
    /// keeps scrolling without needing to move (issue #213). Stops as soon
    /// as `activity` is stale (a newer call to `set_text_autoscroll` ran) or
    /// a tick reaches the start/end of the document and stops moving, rather
    /// than ticking forever once there is nothing left to extend.
    fn spawn_text_autoscroll(
        &mut self,
        direction: AutoscrollDirection,
        activity: u64,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        cx.spawn_in(window, async move |view, cx| {
            loop {
                gpui::Timer::after(TEXT_SELECTION_AUTOSCROLL_INTERVAL).await;
                let Ok(should_continue) = view.update_in(cx, |view, window, cx| {
                    view.step_text_autoscroll(direction, activity, window, cx)
                }) else {
                    return;
                };
                if !should_continue {
                    return;
                }
            }
        })
        .detach();
    }

    /// One autoscroll tick. Returns whether the loop driving it should keep
    /// ticking.
    fn step_text_autoscroll(
        &mut self,
        direction: AutoscrollDirection,
        activity: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.text_autoscroll_activity != activity || self.text_autoscroll != Some(direction) {
            return false;
        }
        let before = self.editor().selection().active;
        match direction {
            AutoscrollDirection::Up => self.move_vertical(false, true, window, cx),
            AutoscrollDirection::Down => self.move_vertical(true, true, window, cx),
        }
        let changed = self.editor().selection().active != before;
        if !changed {
            // Reaching the document edge is a terminal condition for the
            // current hold. Clear the direction as well as ending the task so
            // a later move into the edge zone can start a fresh loop.
            self.set_text_autoscroll(None, window, cx);
        }
        changed
    }

    fn finish_panel_drag(&mut self, _: &MouseUpEvent, window: &mut Window, cx: &mut Context<Self>) {
        let was_sidebar_scrollbar_drag = self.sidebar_scrollbar_drag.take().is_some();
        let was_dragging = self.sidebar_resize_drag.take().is_some()
            || was_sidebar_scrollbar_drag
            || self.editor_scrollbar_drag.take().is_some();
        self.text_selection_drag = false;
        self.set_text_autoscroll(None, window, cx);
        if was_dragging {
            cx.stop_propagation();
            cx.notify();
        }
        if was_sidebar_scrollbar_drag {
            // Keep the just-dragged thumb visible for the same brief window
            // as a wheel scroll, rather than snapping it away the instant
            // the mouse comes up.
            self.show_sidebar_scrollbar_briefly(cx);
        }
    }

    /// Shows the sidebar's overlay scrollbar thumb, then hides it again after
    /// `SIDEBAR_SCROLLBAR_HIDE_DELAY` unless a later scroll or drag
    /// (identified by `sidebar_scrollbar_activity`) supersedes this call
    /// first. Called on sidebar wheel/trackpad scrolling and at the end of a
    /// thumb drag; the idle sidebar shows no track or thumb, only the
    /// existing thin `sidebar_resizer` boundary line.
    fn show_sidebar_scrollbar_briefly(&mut self, cx: &mut Context<Self>) {
        self.sidebar_scrollbar_visible = true;
        self.sidebar_scrollbar_activity = self.sidebar_scrollbar_activity.wrapping_add(1);
        let activity = self.sidebar_scrollbar_activity;
        cx.spawn(async move |view, cx| {
            gpui::Timer::after(SIDEBAR_SCROLLBAR_HIDE_DELAY).await;
            let _ = view.update(cx, |view, cx| {
                if view.sidebar_scrollbar_activity == activity {
                    view.sidebar_scrollbar_visible = false;
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    fn sidebar_resizer(&self, cx: &mut Context<Self>) -> gpui::Stateful<gpui::Div> {
        div()
            .id("work-folder-resizer")
            .flex_none()
            .w(px(SIDEBAR_RESIZER_WIDTH))
            .h_full()
            .flex()
            .justify_center()
            .cursor(CursorStyle::ResizeLeftRight)
            .bg(rgb(self.theme.sidebar_background))
            .child(
                div()
                    .w(px(1.0))
                    .h_full()
                    .bg(rgb(self.theme.sidebar_active_background)),
            )
            .on_mouse_down(MouseButton::Left, cx.listener(Self::begin_sidebar_resize))
    }

    fn editor_scrollbar(
        &self,
        content_height: f32,
        cx: &mut Context<Self>,
    ) -> Option<gpui::Stateful<gpui::Div>> {
        let (top, thumb_height) =
            scrollbar_thumb_geometry(self.viewport_height, content_height, self.scroll_y)?;
        Some(
            div()
                .id("editor-scrollbar")
                .absolute()
                .top(px(0.0))
                .right(px(0.0))
                .w(px(SCROLLBAR_TRACK_WIDTH))
                .h(px(self.viewport_height))
                .bg(rgb(self.theme.code_background))
                .child(
                    div()
                        .id("editor-scrollbar-thumb")
                        .absolute()
                        .top(px(top))
                        .right(px((SCROLLBAR_TRACK_WIDTH - SCROLLBAR_THUMB_WIDTH) / 2.0))
                        .w(px(SCROLLBAR_THUMB_WIDTH))
                        .h(px(thumb_height))
                        .rounded_sm()
                        .cursor(CursorStyle::OpenHand)
                        .bg(rgb(self.theme.quote_foreground))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |view, event, window, cx| {
                                view.begin_editor_scrollbar_drag(event, content_height, window, cx);
                            }),
                        ),
                ),
        )
    }

    fn sidebar_scrollbar(
        &self,
        list_top: f32,
        viewport_height: f32,
        content_height: f32,
        cx: &mut Context<Self>,
    ) -> Option<gpui::Stateful<gpui::Div>> {
        let max_scroll = (content_height - viewport_height).max(0.0);
        let scroll_y = (-f32::from(self.sidebar_scroll.offset().y)).clamp(0.0, max_scroll);
        let (thumb_top, thumb_height) =
            scrollbar_thumb_geometry(viewport_height, content_height, scroll_y)?;
        // Idle sidebar shows no track or thumb at all: the only visible
        // right-edge boundary is the thin `sidebar_resizer` line. The thumb
        // appears only while actively scrolling or being dragged.
        if !self.sidebar_scrollbar_visible && self.sidebar_scrollbar_drag.is_none() {
            return None;
        }
        Some(
            div()
                .id("work-folder-scrollbar")
                .absolute()
                .top(px(list_top))
                .right(px(0.0))
                .w(px(SCROLLBAR_TRACK_WIDTH))
                .h(px(viewport_height))
                .child(
                    div()
                        .id("work-folder-scrollbar-thumb")
                        .absolute()
                        .top(px(thumb_top))
                        .right(px(
                            (SCROLLBAR_TRACK_WIDTH - SIDEBAR_SCROLLBAR_THUMB_WIDTH) / 2.0
                        ))
                        .w(px(SIDEBAR_SCROLLBAR_THUMB_WIDTH))
                        .h(px(thumb_height))
                        .rounded_sm()
                        .cursor(CursorStyle::OpenHand)
                        .bg(rgb(self.theme.quote_foreground))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |view, event, window, cx| {
                                view.begin_sidebar_scrollbar_drag(
                                    event,
                                    viewport_height,
                                    content_height,
                                    window,
                                    cx,
                                );
                            }),
                        ),
                ),
        )
    }
}

#[cfg(test)]
mod panel_layout_tests {
    use super::*;

    #[test]
    fn scrollbar_thumb_tracks_scroll_fraction() {
        let (top, height) = scrollbar_thumb_geometry(100.0, 400.0, 150.0).unwrap();
        assert_eq!(height, 28.0);
        assert_eq!(top, 36.0);
        assert_eq!(scroll_y_for_thumb_drag(0.0, 72.0, 100.0, 400.0), 300.0);
    }

    #[test]
    fn editor_scrollbar_drag_preserves_document_end_badge_clearance() {
        let document_height = 400.0;
        let viewport_height = 100.0;
        let content_height = document_height + CARET_MODE_BADGE_HEIGHT;

        let (_, thumb_height) =
            scrollbar_thumb_geometry(viewport_height, content_height, 0.0).unwrap();
        let end_scroll =
            scroll_y_for_thumb_drag(0.0, viewport_height, viewport_height, content_height);

        assert_eq!(end_scroll, content_height - viewport_height);
        let (thumb_top, _) =
            scrollbar_thumb_geometry(viewport_height, content_height, end_scroll).unwrap();
        assert!((thumb_top + thumb_height - viewport_height).abs() < f32::EPSILON);

        // The last document row ends before the viewport by exactly the
        // badge footprint, even when the position was reached by dragging the
        // editor scrollbar rather than by caret tracking or wheel scrolling.
        let document_bottom_in_viewport = document_height - end_scroll;
        assert_eq!(
            document_bottom_in_viewport + CARET_MODE_BADGE_HEIGHT,
            viewport_height
        );
    }

    #[test]
    fn scrollbar_is_hidden_when_content_fits() {
        assert_eq!(scrollbar_thumb_geometry(100.0, 100.0, 0.0), None);
        assert_eq!(scrollbar_thumb_geometry(100.0, 80.0, 0.0), None);
    }

    #[test]
    fn sidebar_width_drag_respects_panel_limits() {
        assert_eq!(sidebar_width_for_drag(220.0, -500.0, 1200.0), 160.0);
        assert_eq!(sidebar_width_for_drag(220.0, 500.0, 1200.0), 480.0);
        assert_eq!(sidebar_width_for_drag(220.0, 40.0, 1200.0), 260.0);
    }
}

impl Render for EditorView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let layout_started = Instant::now();
        if self._input_mode_focus_subscription.is_none() {
            let focus_handle = self.focus_handle.clone();
            self._input_mode_focus_subscription =
                Some(cx.on_focus(&focus_handle, window, |view, _, cx| {
                    view.caret_input_mode = gpui::active_keyboard_input_mode();
                    cx.notify();
                }));
        }
        let resolved_theme = resolve_theme(self.settings.theme, window.appearance());
        if resolved_theme != self.theme {
            self.theme = resolved_theme;
            self.block_cache.clear();
            self.layout_cache.clear();
            let (granularity, _) = self.desired_layout();
            let heights = HeightIndex::new(self.item_heights());
            self.install_heights(granularity, heights);
        }
        self.schedule_document_parse(cx);
        self.viewport_height = (f32::from(window.viewport_size().height)
            - self.theme.header_height)
            .max(self.line_height());
        self.step_measurement_scroll(window);
        // The width of the text column decides where every row breaks, so it is
        // read once per frame and every layout is keyed by it. A sidebar takes
        // its width out of the same window, so it must be subtracted here too,
        // not just in the element tree, or wrapping would be computed for a
        // column wider than what is actually drawn.
        let sidebar_width = if self.work_folder.is_some() {
            self.sidebar_width + SIDEBAR_RESIZER_WIDTH
        } else {
            0.0
        };
        self.main_column_left = sidebar_width;
        self.content_width = text_column_width(
            f32::from(window.viewport_size().width),
            sidebar_width,
            self.theme.line_horizontal_padding,
        );
        let shaper = WindowShaper::new(window, self.zoom);
        let font_revision = shaper.font_revision();
        if font_revision != self.layout_font_revision {
            self.layout_font_revision = font_revision;
            // `VisualLine::estimated_height` is part of the presentation, not
            // just the shaped layout. Reusing it across zoom generations
            // leaves the new glyphs with the old row height until a later
            // cache miss, which can clip or overlap text.
            self.block_cache.clear();
            self.layout_cache.clear();
            let (granularity, _) = self.desired_layout();
            let heights = HeightIndex::new(self.item_heights());
            self.install_heights(granularity, heights);
        }
        self.scroll_y = clamp_scroll_y(
            self.scroll_y,
            self.scrollable_content_height(),
            self.viewport_height,
        );
        let visible =
            self.heights
                .visible_range(self.scroll_y, self.viewport_height, self.theme.overscan);
        let blocks = self.visible_blocks(visible.clone());
        self.schedule_joined_parse(&blocks, cx);
        // Keep a margin of presentations around the viewport so scrolling back
        // does not re-present, and drop the rest.
        let retained = self
            .current_index()
            .map(|index| {
                (visible.start.saturating_sub(BLOCK_CACHE_MARGIN)..visible.end + BLOCK_CACHE_MARGIN)
                    .filter_map(|ordinal| index.block(ordinal))
                    .map(|block| block.id)
                    .collect::<HashSet<_>>()
            })
            .unwrap_or_else(|| blocks.iter().map(|block| block.id).collect());
        self.block_cache.retain(|id, _| retained.contains(id));

        self.layout_cache.retain(|id, _| retained.contains(id));
        self.joined_parse_cache
            .retain(|id, _| retained.contains(id));

        let lines = self.visible_line_window(&blocks, &visible);
        let mut rendered = Vec::with_capacity(blocks.len());
        for block in &blocks {
            if let Some((visual, reused)) = self.cached_block(block, &lines) {
                let layout = self.block_layout(&visual, reused, &shaper);
                rendered.push((block.ordinal, visual, layout));
            }
        }
        // Height entries follow whatever the index is keyed by: one per block
        // once the document has been parsed, one per physical line until then.
        // Either way the height is the laid-out one, so a wrapped line takes the
        // room its rows actually need.
        let mut first_item = usize::MAX;
        let mut last_item = 0;
        let height_anchor = (self.granularity == Granularity::Blocks && !self.heights.is_empty())
            .then(|| {
                let ordinal = self.heights.block_at_y(self.scroll_y);
                (ordinal, self.scroll_y - self.heights.prefix_sum(ordinal))
            });
        self.line_owners.clear();
        for (ordinal, visual, layout) in &rendered {
            match self.granularity {
                Granularity::Blocks => {
                    if *ordinal < self.heights.len() {
                        self.heights.update(*ordinal, layout.height());
                    }
                    first_item = first_item.min(*ordinal);
                    last_item = last_item.max(ordinal + 1);
                }
                Granularity::Lines => {
                    for (at, line) in visual.lines.iter().enumerate() {
                        let line_id = line.line_id as usize;
                        if line_id < self.heights.len() {
                            self.heights.update(line_id, layout.line_height_of(at));
                        }
                        first_item = first_item.min(line_id);
                        last_item = last_item.max(line_id + 1);
                    }
                }
            }
            for (at, line) in visual.lines.iter().enumerate() {
                self.line_owners
                    .insert(line.line_id as usize, (visual.id, at));
            }
        }
        // A zoom gesture is anchored after these visible blocks have been
        // remeasured. Resolving it earlier against estimated heights is wrong
        // for a soft-wrapped paragraph: the measured block height can differ by
        // several rows in the same frame. The next frame is notified below if
        // the corrected anchor changed which blocks should be visible.
        let zoom_anchor_applied = self
            .pending_zoom_anchor
            .take()
            .filter(|_| !self.heights.is_empty())
            .map(|anchor| {
                self.scroll_y = self.scroll_y_for_zoom_anchor(anchor);
                true
            })
            .unwrap_or(false);
        if zoom_anchor_applied {
            // The visible block set was selected before measured heights were
            // installed. Ask GPUI for one more frame so virtualization and
            // spacers are recomputed from the corrected anchor position.
            cx.notify();
        }
        // Measuring wrapped rows can correct blocks above the viewport. Keep the
        // same block and the same position inside it at the top instead of
        // letting those corrections visibly move the document, unless the
        // zoom gesture has a more specific pointer/pinch anchor.
        if !zoom_anchor_applied && let Some((old_ordinal, intra)) = height_anchor {
            let ordinal = old_ordinal.min(self.heights.len().saturating_sub(1));
            let inside = if ordinal + 1 == self.heights.len() {
                // At the document's last item, `intra` can already be
                // carrying the `CARET_MODE_BADGE_HEIGHT` clearance
                // `scroll_cursor_into_view` reserved past its bottom.
                // Clamping it to the item's own height would throw that
                // clearance away before the badge is ever drawn (issue
                // #240); the clamp below still bounds the result.
                intra.max(0.0)
            } else {
                self.heights
                    .height(ordinal)
                    .map_or(0.0, |height| intra.clamp(0.0, height))
            };
            self.scroll_y = self.heights.prefix_sum(ordinal) + inside;
        }
        // A newly measured block can shrink at the old bottom. Anchoring
        // preserves its block-relative position, which can now sit below the
        // new scroll limit and move all content above the viewport.
        self.scroll_y = clamp_scroll_y(
            self.scroll_y,
            self.scrollable_content_height(),
            self.viewport_height,
        );
        // Resolve the caret from the layouts produced in this frame, not from
        // a cache that may still describe the pre-disclosure zero-height row.
        // A fence can become editable one frame after the input event; keep the
        // request armed until that row has a real positive height.
        let caret = self.editor().selection().active;
        let caret_line = self.editor().document().line_for_offset(caret).ok();
        let fresh_caret = rendered.iter().find_map(|(ordinal, visual, layout)| {
            if caret < visual.source_range.start || visual.source_range.end < caret {
                return None;
            }
            let point = layout.point_for_source(visual, caret, &shaper)?;
            let top = match self.granularity {
                Granularity::Blocks => self.heights.prefix_sum(*ordinal) + point.y,
                Granularity::Lines => {
                    let line = caret_line?;
                    let line_id = line.0;
                    let visual_line = visual
                        .lines
                        .iter()
                        .position(|line| line.line_id as usize == line_id)?;
                    let line_row_top = layout
                        .lines
                        .iter()
                        .find(|row| row.line == visual_line)
                        .map(|row| row.y)?;
                    self.heights.prefix_sum(line.0) + point.y - line_row_top
                }
            };
            Some((point.x, top, point.height))
        });
        if self.pending_caret_visibility_after_layout {
            if let Some((_, top, height)) = fresh_caret && height > 0.0 {
                let before = self.scroll_y;
                self.scroll_y = scroll_y_for_cursor(
                    self.scroll_y,
                    top,
                    height + CARET_MODE_BADGE_HEIGHT,
                    self.viewport_height,
                );
                self.scroll_y = clamp_scroll_y(
                    self.scroll_y,
                    self.scrollable_content_height(),
                    self.viewport_height,
                );
                let visible_bottom =
                    top + height + CARET_MODE_BADGE_HEIGHT - self.scroll_y;
                if visible_bottom <= self.viewport_height + CARET_VISIBILITY_TOLERANCE
                    && self.scroll_y == before
                {
                    self.pending_caret_visibility_after_layout = false;
                } else {
                    // Keep the request armed for one more frame whenever this
                    // correction moved the scroll. A following frame may apply
                    // the height anchor to newly measured rows; clearing here
                    // would let that anchor erase the badge clearance before
                    // the caret has reached a stable layout.
                    cx.notify();
                }
                if self.scroll_y != before {
                    // The visible block set was selected before this corrected
                    // position. One more frame lets virtualization follow it.
                    cx.notify();
                }
            } else {
                // The fresh disclosed row has not been laid out yet. Keep the
                // request alive for the next frame instead of consuming it
                // against stale zero-height geometry.
                cx.notify();
            }
        }
        // Where the caret was drawn, for the IME candidate window.
        self.caret_geometry = fresh_caret.map(|(x, top, height)| CaretGeometry {
            x: self.theme.line_horizontal_padding + x,
            y: top - self.scroll_y,
            height,
        });
        // Blocks are drawn whole, so the rendered span can start above the
        // viewport; the spacers have to match what was actually drawn.
        let items = if first_item < last_item {
            first_item..last_item
        } else {
            visible.clone()
        };
        let top_space = self.heights.prefix_sum(items.start);
        let bottom_space =
            (self.heights.total_height() - self.heights.prefix_sum(items.end)).max(0.0);
        let revision = self.editor().document().revision().0;
        let bytes = self.editor().document().len_bytes().0;
        let p95 = self
            .metrics
            .painted_percentile(0.95)
            .map_or(0.0, |duration| duration.as_secs_f64() * 1000.0);
        let background = if self.background_presentation_generation > 0 {
            format!(" · bg {}", self.background_presentation_generation)
        } else {
            String::new()
        };
        let dirty = if self.sessions.active().is_dirty() {
            " · modified"
        } else {
            ""
        };
        let status = header_status_line(
            self.draft_recovery_warning.as_deref(),
            self.status.as_deref(),
        )
        .unwrap_or_else(|| {
            format!("rev {revision} · {bytes} bytes · frame p95 {p95:.2} ms{background}{dirty}")
        });

        let root = div()
            .size_full()
            .flex()
            .flex_row()
            .bg(rgb(self.theme.editor_background))
            .text_color(rgb(self.theme.foreground))
            .key_context("HaneEditor")
            .track_focus(&self.focus_handle(cx));
        let sidebar_viewport_height = f32::from(window.viewport_size().height);
        let sidebar = self.work_folder_sidebar(sidebar_viewport_height, cx);
        let resizer = if self.work_folder.is_some() {
            Some(self.sidebar_resizer(cx))
        } else {
            None
        };
        let root = install_action_listeners(root, cx)
            .children(sidebar)
            .children(resizer)
            .on_mouse_move(cx.listener(Self::on_panel_mouse_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::finish_panel_drag))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::finish_panel_drag));
        let main_column = div()
            .flex_1()
            .min_w(px(0.0))
            .flex()
            .flex_col()
            .child(self.header_element(status, cx));
        // Relative image destinations resolve against the session's own file,
        // never against the directory the process happens to run in.
        let resolver = self.sessions.active().resource_resolver();
        let editor = self.sessions.active().editor();
        let editor_scrollbar = self.editor_scrollbar(self.scrollable_content_height(), cx);
        let main_column = main_column.child(
            div()
                .relative()
                .flex_1()
                .overflow_hidden()
                .on_mouse_down(MouseButton::Left, cx.listener(Self::on_editor_mouse_down))
                .on_scroll_wheel(cx.listener(Self::on_scroll))
                .on_magnify(cx.listener(Self::on_magnify))
                .child(InputCapture { input: cx.entity() })
                .child(
                    div()
                        .absolute()
                        .top(px(content_top_for_scroll(self.scroll_y)))
                        .flex()
                        .flex_col()
                        .w_full()
                        .child(div().h(px(top_space)))
                        // One element per Markdown block, and inside it one per
                        // row: what is generated scales with visible blocks, and
                        // within a block with the rows that reach the viewport.
                        .children(rendered.into_iter().map(|(_, visual, layout)| {
                            block_element(
                                &layout,
                                (0..layout.lines.len()).map(|row_index| {
                                    let row = &layout.lines[row_index];
                                    let line = row.line_id as usize;
                                    let fragment = row.line_visual_range.clone();
                                    let dragged = fragment.clone();
                                    row_element(
                                        editor,
                                        &visual,
                                        &layout,
                                        row_index,
                                        self.theme,
                                        self.zoom,
                                        &resolver,
                                        self.caret_input_mode,
                                    )
                                    // Lets GPUI-event regression tests read a row's real
                                    // painted window bounds via `VisualTestContext::debug_bounds`
                                    // instead of duplicating the render-time offset math. A
                                    // no-op outside test builds.
                                    .debug_selector(move || format!("row-{line}-{row_index}"))
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(move |view, event, window, cx| {
                                            view.on_row_mouse_down(
                                                line,
                                                fragment.clone(),
                                                event,
                                                window,
                                                cx,
                                            )
                                        }),
                                    )
                                    .on_mouse_move(
                                        cx.listener(move |view, event, window, cx| {
                                            view.on_row_mouse_move(
                                                line,
                                                dragged.clone(),
                                                event,
                                                window,
                                                cx,
                                            )
                                        }),
                                    )
                                }),
                            )
                        }))
                        .child(div().h(px(bottom_space))),
                )
                .children(editor_scrollbar),
        );
        let rendered = root.child(main_column);
        self.metrics.record_layout(layout_started.elapsed());
        rendered
    }
}

/// One row of the flattened sidebar tree: a node together with how deeply it
/// is nested, so indentation can be applied without recursive rendering.
struct WorkFolderRow<'a> {
    depth: usize,
    node: &'a WorkFolderNode,
}

/// Flattens the tree into display order (depth-first, each level already
/// sorted by `WorkFolder`/`WorkFolderFolder`), descending into a folder only
/// when it is in `expanded`. The work folder root itself is always expanded.
fn flatten_work_folder_tree<'a>(
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
fn flatten_filtered_work_folder_tree<'a>(
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
    fn refresh_sidebar_date_badge_today(&mut self, cx: &mut Context<Self>) {
        self.apply_sidebar_date_badge_today(local_today(), cx);
    }

    /// The state update `refresh_sidebar_date_badge_today` drives, split out
    /// so the "did today actually change" decision is unit-testable without
    /// depending on the system clock or a real timer. Returns whether
    /// `today` differed from what the sidebar last used; `cx.notify()` only
    /// fires in that case, so a recheck that lands on the same day is a
    /// no-op redraw-wise.
    fn apply_sidebar_date_badge_today(
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
    fn work_folder_sidebar(
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
                window.focus(&view.focus_handle);
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
                                window.focus(&view.focus_handle);
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
                                    window.focus(&view.focus_handle);
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
                                window.focus(&view.focus_handle);
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
                                    window.focus(&view.focus_handle);
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
        let content_height = sidebar_list_content_height(
            tree_row_count,
            draft_row_count,
            empty_filter_row,
        );
        let list_top = SIDEBAR_PADDING
            + SIDEBAR_TOOLBAR_HEIGHT
            + SIDEBAR_TOOLBAR_GAP
            + if show_filter {
                SIDEBAR_FILTER_HEIGHT + SIDEBAR_FILTER_GAP
            } else {
                0.0
            };
        let list_viewport_height = (sidebar_viewport_height - list_top - SIDEBAR_PADDING).max(0.0);
        let scrollbar = self.sidebar_scrollbar(list_top, list_viewport_height, content_height, cx);
        let list = div()
            .id("work-folder-sidebar-list")
            .debug_selector(|| "sidebar-list".to_owned())
            .flex_1()
            .overflow_y_scroll()
            .track_scroll(&self.sidebar_scroll)
            .on_scroll_wheel(
                cx.listener(|view, _, _, cx| view.show_sidebar_scrollbar_briefly(cx)),
            )
            .child(root_row)
            .children(tree)
            .children(empty_filter)
            .children(drafts);
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
                        .child(list),
                )
                .children(scrollbar),
        )
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

impl EditorView {
    fn header_element(&self, status: String, cx: &mut Context<Self>) -> gpui::Div {
        let autosave = if self.settings.autosave {
            "Autosave on"
        } else {
            "Autosave off"
        };
        let theme = format!("Theme {:?}", self.settings.theme);
        let active_path = self.sessions.active().path();
        let recent = self
            .recent
            .entries()
            .iter()
            .filter(|path| Some(path.as_path()) != active_path)
            .take(3)
            .cloned()
            .enumerate()
            .map(|(index, path)| {
                let label = path.file_name().map_or_else(
                    || path.display().to_string(),
                    |name| name.to_string_lossy().into_owned(),
                );
                div()
                    .id(("recent-file", index))
                    .px_2()
                    .rounded_sm()
                    .bg(rgb(self.theme.code_background))
                    .text_color(rgb(self.theme.foreground))
                    .cursor_pointer()
                    .child(label)
                    .on_click(cx.listener(move |view, _, _, cx| view.open_path(&path, cx)))
            })
            .collect::<Vec<_>>();
        div()
            .h(px(self.theme.header_height))
            .flex_none()
            .flex()
            .flex_col()
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_editor_mouse_down))
            .bg(rgb(self.theme.header_background))
            .text_color(rgb(self.theme.header_foreground))
            .child(
                div()
                    .h(px(38.0))
                    .flex()
                    .items_center()
                    .justify_between()
                    .px_3()
                    .child(self.sessions.active().label())
                    .child(status),
            )
            .child(
                div()
                    .h(px(30.0))
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .text_size(px(11.0))
                    .child(
                        div()
                            .id("toggle-autosave")
                            .cursor_pointer()
                            .child(autosave)
                            .on_click(cx.listener(|view, _, _, cx| view.toggle_autosave(cx))),
                    )
                    .child(
                        div()
                            .id("cycle-theme")
                            .cursor_pointer()
                            .child(theme)
                            .on_click(
                                cx.listener(|view, _, window, cx| view.cycle_theme(window, cx)),
                            ),
                    )
                    .child("Recent:")
                    .children(recent),
            )
    }
}

/// `fragment` is the clicked row's own stretch of `block`'s visual text (see
/// `EditorView::offset_at_row_x`), when the caller is resolving a click on a
/// specific rendered row rather than a bare visual offset. It lets
/// `collapsed_boundary_bias` tell which side of a soft-wrapped, zero-width
/// collapsed boundary the click actually landed on; `None` (as from the
/// non-row test helpers below) leaves that disambiguation off, matching the
/// unwrapped, whole-line behavior.
#[cfg(test)]
fn source_offset_for_visual_position(
    editor: &Editor,
    line: usize,
    block: &VisualLine,
    visual_offset: usize,
    fragment: Option<&Range<usize>>,
) -> SourceOffset {
    block
        .source_map
        .visual_to_source(
            VisualOffset(visual_offset),
            collapsed_boundary_bias(block, visual_offset, fragment),
        )
        .map(|candidate| candidate.source_offset)
        .or_else(|| {
            editor
                .document()
                .line_content_range(LineId(line))
                .ok()
                .map(|range| range.start)
        })
        .unwrap_or(block.source_range.start)
}

/// A hidden marker (e.g. the trailing `**` of bold text, or the opening `` ` ``
/// of a code span) collapses to zero visual width, so its own visual position
/// is indistinguishable from the visible text on whichever side is unrelated
/// content. Per the collapsed-boundary contract (ADR-0004), a click there must
/// land on the visible content the marker actually belongs to: just before it
/// for a closing marker (content it closes sits to the left), just after it
/// for an opening marker (content it opens sits to the right). `MarkerEdge`
/// carries that distinction from the parse tree, including for a quote/list
/// prefix, whose owning container node always resolves it as opening even
/// though the node's own source range can start before the marker (at the
/// container's indentation). A marker with no resolvable edge at all (not
/// expected for markers this crate derives) keeps the closing-side default.
///
/// Two adjacent constructs with no visible gap (e.g. `[a](x)[b](y)`) collapse
/// a closing marker and the next construct's opening marker onto the very
/// same point. Picking whichever of them happens to appear first at that
/// point would arbitrarily favor the closing side; instead an opening edge
/// anywhere at the point wins, since a click there is visually at the start
/// of the following visible content, not the end of the preceding content.
///
/// A soft-wrapped row boundary can land on exactly the same collapsed point:
/// the row that ends there and the row that starts there both render text
/// whose shared visual offset is this marker's position. `marker_edge` alone
/// cannot tell those two rows apart — it only knows the marker's own
/// direction, not which side of it the click actually hit — so a click at the
/// start of the row that begins the boundary always resolved to the
/// upper-row (closing-marker) side even when the click was on the next row's
/// own visible text. `fragment`, the clicked row's own stretch of `block`'s
/// visual text, resolves that: a click exactly at the row's leading edge (and
/// not at the very start of the line, which is not a wrap boundary) belongs
/// to the content that row opens with; a click exactly at the row's trailing
/// edge (and not at the very end of the line, which owns the caret itself as
/// a hard-wrapped row's own trailing position) belongs to the content that
/// row closes with. This is checked before the marker-edge fallback because
/// it reflects where the click physically landed, which the marker's own
/// direction cannot.
fn collapsed_boundary_bias(
    block: &VisualLine,
    visual_offset: usize,
    fragment: Option<&Range<usize>>,
) -> Bias {
    if let Some(fragment) = fragment {
        if fragment.start == visual_offset && visual_offset > 0 {
            return Bias::After;
        }
        if fragment.end == visual_offset && visual_offset < block.visual_text.len() {
            return Bias::Before;
        }
    }
    let mut edges = block.source_map.segments.iter().filter_map(|segment| {
        let at_point = segment.visual_range.start.0 == visual_offset
            && segment.visual_range.end.0 == visual_offset;
        (at_point && segment.visibility == Visibility::HiddenMarkup)
            .then_some(segment.marker_edge)
            .flatten()
    });
    if edges.any(|edge| edge == MarkerEdge::Opening) {
        Bias::After
    } else {
        Bias::Before
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::line::JOIN_SYNC_LINE_BUDGET;
    use hane_document::LineId;
    use hane_presentation::testing::FixedAdvanceShaper;
    use hane_session::RecoveredDraft;

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
    fn layout_cache_key_rejects_each_geometry_input_independently() {
        let entry = LayoutCacheEntry {
            layout: BlockLayout {
                block: BlockId(7),
                revision: Revision(3),
                width: 640.0,
                lines: Vec::new(),
                leading_space: 0.0,
                trailing_space: 0.0,
            },
            font_revision: 11,
        };
        assert!(entry.is_valid(640.0, 11, Revision(3)));
        assert!(!entry.is_valid(639.0, 11, Revision(3)));
        assert!(!entry.is_valid(640.0, 12, Revision(3)));
        assert!(!entry.is_valid(640.0, 11, Revision(4)));
    }

    #[test]
    fn the_date_badge_renders_before_the_remainder_only_on_the_left() {
        assert!(badge_renders_before_remainder(DateBadgePosition::Left));
        assert!(!badge_renders_before_remainder(DateBadgePosition::Right));
    }

    #[test]
    fn inline_rename_keeps_markdown_extensions_out_of_the_editable_text() {
        assert_eq!(
            inline_rename_parts(Path::new("Meeting 2026-09-19.md"), InlineRenameKind::File),
            Some(("Meeting 2026-09-19".to_owned(), Some(".md".to_owned())))
        );
        assert_eq!(
            inline_rename_parts(Path::new("Upper.MD"), InlineRenameKind::File),
            Some(("Upper".to_owned(), Some(".MD".to_owned())))
        );
        assert_eq!(
            inline_rename_parts(Path::new("folder"), InlineRenameKind::Folder),
            Some(("folder".to_owned(), None))
        );
    }

    #[test]
    #[cfg(unix)]
    fn inline_rename_refuses_non_utf8_names_instead_of_replacing_bytes() {
        use std::os::unix::ffi::OsStringExt;

        let name = std::ffi::OsString::from_vec(vec![b'B', 0x80, b'.', b'm', b'd']);
        let path = PathBuf::from(name);
        assert_eq!(
            inline_rename_parts(&path, InlineRenameKind::File),
            None,
            "non-UTF-8 names must not enter a lossy text field"
        );
    }

    #[test]
    fn inline_rename_movement_uses_grapheme_boundaries() {
        let text = "a\u{301}👩‍💻z";
        let combining_end = "a\u{301}".len();
        let emoji_end = "a\u{301}👩‍💻".len();

        assert_eq!(next_inline_rename_boundary(text, 0), combining_end);
        assert_eq!(previous_inline_rename_boundary(text, combining_end), 0);
        assert_eq!(next_inline_rename_boundary(text, combining_end), emoji_end);
        assert_eq!(
            previous_inline_rename_boundary(text, emoji_end),
            combining_end
        );
        assert_eq!(next_inline_rename_boundary(text, emoji_end), text.len());
    }

    #[test]
    fn inline_rename_shift_home_and_end_extend_from_the_active_caret() {
        let mut forward = InlineRename {
            kind: InlineRenameKind::File,
            from: PathBuf::from("note.md"),
            text: "abcdef".to_owned(),
            fixed_extension: Some(".md".to_owned()),
            selected_range: 2..4,
            selection_reversed: false,
            marked_range: None,
            composition: None,
            pending: false,
        };
        select_inline_rename_to(&mut forward, 0);
        assert_eq!(forward.selected_range, 0..2);
        assert!(forward.selection_reversed);

        let mut reversed = InlineRename {
            selection_reversed: true,
            ..forward
        };
        reversed.selected_range = 2..4;
        select_inline_rename_to(&mut reversed, 6);
        assert_eq!(reversed.selected_range, 4..6);
        assert!(!reversed.selection_reversed);
    }

    #[gpui::test]
    fn inline_rename_ime_selection_is_relative_to_the_marked_replacement(
        cx: &mut gpui::TestAppContext,
    ) {
        let root = draft_test_root("inline-rename-ime-relative-selection");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("AlphaBeta.md"), "plain").unwrap();
        let (view, cx) = open_inline_rename_test_view(cx, &root);

        let file_point = cx.debug_bounds("sidebar-file").unwrap().center();
        cx.simulate_click(file_point, gpui::Modifiers::none());
        cx.simulate_keystrokes("f2");
        view.update(cx, |view, cx| {
            assert!(view.replace_and_mark_inline_rename_text(Some(5..5), "日本", Some(1..1), cx,));
            assert_eq!(
                view.inline_rename_selection().map(|(range, _)| range),
                Some(6..6),
                "IME selection is relative to the inserted marked text"
            );
        });

        std::fs::remove_dir_all(root).unwrap();
    }

    #[gpui::test]
    fn inline_rename_escape_cancels_ime_composition_before_the_rename(
        cx: &mut gpui::TestAppContext,
    ) {
        let root = draft_test_root("inline-rename-ime-cancel");
        std::fs::create_dir_all(&root).unwrap();
        let original = root.join("Alpha.md");
        std::fs::write(&original, "plain").unwrap();
        let (view, cx) = open_inline_rename_test_view(cx, &root);

        let file_point = cx.debug_bounds("sidebar-file").unwrap().center();
        cx.simulate_click(file_point, gpui::Modifiers::none());
        cx.simulate_keystrokes("f2");
        view.update(cx, |view, cx| {
            assert!(view.replace_and_mark_inline_rename_text(None, "かな", Some(2..2), cx));
            assert!(view.inline_rename_has_composition());
        });

        cx.simulate_keystrokes("escape");
        view.read_with(cx, |view, _| {
            assert!(view.inline_rename_active());
            assert!(!view.inline_rename_has_composition());
            assert_eq!(
                view.inline_rename_render_state().unwrap().text,
                "Alpha".to_owned()
            );
        });
        assert!(original.exists());
        assert!(!root.join("かな.md").exists());

        cx.simulate_keystrokes("escape");
        view.read_with(cx, |view, _| assert!(!view.inline_rename_active()));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[gpui::test]
    fn inline_rename_enter_commits_ime_composition_before_the_rename(
        cx: &mut gpui::TestAppContext,
    ) {
        let root = draft_test_root("inline-rename-ime-commit");
        std::fs::create_dir_all(&root).unwrap();
        let original = root.join("Alpha.md");
        std::fs::write(&original, "plain").unwrap();
        let (view, cx) = open_inline_rename_test_view(cx, &root);

        let file_point = cx.debug_bounds("sidebar-file").unwrap().center();
        cx.simulate_click(file_point, gpui::Modifiers::none());
        cx.simulate_keystrokes("f2");
        view.update(cx, |view, cx| {
            assert!(view.replace_and_mark_inline_rename_text(None, "日本", Some(2..2), cx));
        });

        cx.simulate_keystrokes("enter");
        view.read_with(cx, |view, _| {
            assert!(view.inline_rename_active());
            assert!(!view.inline_rename_has_composition());
            assert_eq!(
                view.inline_rename_render_state().unwrap().text,
                "日本".to_owned()
            );
        });
        assert!(original.exists());
        assert!(!root.join("日本.md").exists());

        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        assert!(!original.exists());
        assert!(root.join("日本.md").exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn inline_rename_rejects_names_that_escape_the_parent_directory() {
        for name in ["", ".", "..", "folder/name", r"folder\name"] {
            assert!(!valid_inline_rename_name(name), "{name:?} must be rejected");
        }
        assert!(valid_inline_rename_name("Meeting 2026-09-19"));
    }

    #[test]
    fn inline_rename_ime_selection_is_relative_to_the_inserted_text() {
        assert_eq!(
            inline_rename_selected_range(6, "かな", Some(1..2), 12..12),
            9..12
        );
        assert_eq!(
            inline_rename_selected_range(6, "かな", None, 12..12),
            12..12
        );
    }

    #[test]
    fn folder_rename_path_rebasing_preserves_unrelated_ui_paths() {
        let root = Path::new("/tmp/hane-217");
        let from = root.join("Project");
        let to = root.join("Renamed");
        assert_eq!(
            rebase_ui_path(&from.join("Deep/Child.md"), &from, &to),
            Some(to.join("Deep/Child.md"))
        );
        assert_eq!(rebase_ui_path(&root.join("Other"), &from, &to), None);
    }

    // Regression coverage for the sidebar's `本日` badge going stale across a
    // local-midnight boundary while the window stays open: `gpui::Timer` is
    // wall-clock in this codebase's tests (see
    // `draft_save_survives_switching_sessions_within_the_debounce_window`),
    // so waiting out `DATE_BADGE_REFRESH_INTERVAL` for real is not practical
    // here. This instead drives the exact state-update the periodic task
    // calls on every tick, which is what actually decides whether the
    // sidebar redraws.
    #[gpui::test]
    fn apply_sidebar_date_badge_today_notifies_only_when_the_date_actually_changes(
        cx: &mut gpui::TestAppContext,
    ) {
        let view = gpui::AppContext::new(cx, |cx| EditorView::new("", "Untitled", cx));
        let initial = view.update(cx, |view, _cx| view.sidebar_date_badge_today);
        let other_day = CalendarDate::new(1, 1, 1).unwrap();
        assert_ne!(initial, other_day);

        // Rechecking the same day the sidebar already knows about must not
        // report a change: nothing new to redraw.
        let changed = view.update(cx, |view, cx| {
            view.apply_sidebar_date_badge_today(initial, cx)
        });
        assert!(!changed);
        assert_eq!(
            view.update(cx, |view, _cx| view.sidebar_date_badge_today),
            initial
        );

        // A genuine date-boundary crossing updates the cached date and
        // reports that the sidebar has something new to show.
        let changed = view.update(cx, |view, cx| {
            view.apply_sidebar_date_badge_today(other_day, cx)
        });
        assert!(changed);
        assert_eq!(
            view.update(cx, |view, _cx| view.sidebar_date_badge_today),
            other_day
        );

        // Rechecking again on the new day is once more a no-op.
        let changed_again = view.update(cx, |view, cx| {
            view.apply_sidebar_date_badge_today(other_day, cx)
        });
        assert!(!changed_again);
    }

    // Issue #195: the sidebar's overlay scrollbar thumb must appear only
    // while the user is actively scrolling, not sit visible at rest.
    #[gpui::test]
    fn sidebar_scrollbar_thumb_shows_on_scroll_and_hides_after_the_delay(
        cx: &mut gpui::TestAppContext,
    ) {
        let view = gpui::AppContext::new(cx, |cx| EditorView::new("", "Untitled", cx));

        view.update(cx, |view, cx| {
            assert!(!view.sidebar_scrollbar_visible);
            view.show_sidebar_scrollbar_briefly(cx);
            assert!(view.sidebar_scrollbar_visible);
        });

        // `show_sidebar_scrollbar_briefly` debounces on a real `gpui::Timer`
        // (wall-clock, not the deterministic test dispatcher, see
        // `draft_save_survives_switching_sessions_within_the_debounce_window`),
        // so the test has to wait for real time to pass.
        cx.run_until_parked();
        std::thread::sleep(SIDEBAR_SCROLLBAR_HIDE_DELAY + Duration::from_millis(200));
        cx.run_until_parked();

        view.update(cx, |view, _cx| {
            assert!(!view.sidebar_scrollbar_visible);
        });
    }

    // Issue #195: the idle sidebar right edge must show no scrollbar track
    // or thumb, only the existing thin `sidebar_resizer` boundary line; the
    // thumb must still render (and keep its existing position/size contract
    // from `scrollbar_thumb_geometry`) while a drag is in progress.
    #[gpui::test]
    fn sidebar_scrollbar_element_is_hidden_at_rest_and_shown_during_a_drag(
        cx: &mut gpui::TestAppContext,
    ) {
        let view = gpui::AppContext::new(cx, |cx| EditorView::new("", "Untitled", cx));

        view.update(cx, |view, cx| {
            assert!(view.sidebar_scrollbar(0.0, 100.0, 400.0, cx).is_none());

            view.sidebar_scrollbar_drag = Some(ScrollbarDrag {
                pointer_y: 0.0,
                scroll_y: 0.0,
                viewport_height: 100.0,
                content_height: 400.0,
            });
            assert!(view.sidebar_scrollbar(0.0, 100.0, 400.0, cx).is_some());
        });
    }

    #[gpui::test]
    fn sidebar_scrolls_rows_without_moving_fixed_controls(cx: &mut gpui::TestAppContext) {
        let root = draft_test_root("fixed-controls-scroll");
        std::fs::create_dir_all(&root).unwrap();
        for index in 0..40 {
            std::fs::write(root.join(format!("Note-{index:02}.md")), "# Note\n").unwrap();
        }
        let (view, cx) = open_inline_rename_test_view(cx, &root);

        let toolbar_before = cx.debug_bounds("sidebar-toolbar").unwrap();
        let filter_before = cx.debug_bounds("sidebar-filter").unwrap();
        let list_before = cx.debug_bounds("sidebar-list").unwrap();
        let root_before = cx.debug_bounds("sidebar-root").unwrap();

        cx.simulate_event(ScrollWheelEvent {
            position: list_before.center(),
            delta: ScrollDelta::Pixels(point(px(0.0), px(-120.0))),
            modifiers: gpui::Modifiers::none(),
            touch_phase: gpui::TouchPhase::Moved,
        });
        cx.run_until_parked();

        assert_eq!(cx.debug_bounds("sidebar-toolbar").unwrap(), toolbar_before);
        assert_eq!(cx.debug_bounds("sidebar-filter").unwrap(), filter_before);
        assert_eq!(cx.debug_bounds("sidebar-list").unwrap(), list_before);
        assert!(cx.debug_bounds("sidebar-root").unwrap().top() < root_before.top());
        view.read_with(cx, |view, _| {
            assert!(view.sidebar_scroll.offset().y < px(0.0));
        });

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn text_column_width_subtracts_sidebar_and_both_paddings() {
        assert_eq!(text_column_width(1000.0, 0.0, 12.0), 1000.0 - 24.0);
        assert_eq!(
            text_column_width(1000.0, 240.0, 12.0),
            1000.0 - 240.0 - 24.0
        );
        // Never collapses to zero or negative, even on a tiny window: a row
        // still needs a positive width to lay out against.
        assert_eq!(text_column_width(10.0, 240.0, 12.0), 1.0);
    }

    /// Presents a whole document the way `render` does: index first, then one
    /// `presented_block` call per block.
    fn presented_lines(editor: &Editor) -> Vec<VisualLine> {
        let index = BlockIndex::from_buffer(editor.document());
        index
            .blocks()
            .flat_map(|block| {
                presented_block_with_list_projection(
                    editor,
                    &block,
                    &(0..usize::MAX),
                    None,
                    index.list_projection(&block),
                    DEFAULT_LINE_HEIGHT,
                )
                .expect("block presents")
                .lines
            })
            .collect()
    }

    #[test]
    fn visual_click_positions_map_back_to_source_offsets() {
        let editor = Editor::new("ab🙂\n\n**bold**");
        let lines = presented_lines(&editor);

        let first = &lines[0];
        assert_eq!(
            source_offset_for_visual_position(&editor, 0, first, 2, None),
            SourceOffset(2)
        );
        assert_eq!(
            source_offset_for_visual_position(&editor, 0, first, first.visual_text.len(), None),
            SourceOffset(6)
        );

        let empty = &lines[1];
        assert_eq!(
            source_offset_for_visual_position(&editor, 1, empty, 0, None),
            SourceOffset(7)
        );

        let bold = &lines[2];
        assert_eq!(
            source_offset_for_visual_position(&editor, 2, bold, 0, None),
            SourceOffset(10)
        );
        assert_eq!(
            source_offset_for_visual_position(&editor, 2, bold, bold.visual_text.len(), None),
            SourceOffset(14)
        );
    }

    // Issue #143: a hidden closing marker collapses to zero visual width, so
    // when visible text immediately follows it on the same line, the marker's
    // own visual position is indistinguishable from that following text's
    // start. The canonical position for a click there must stay on the
    // content the marker closes (just before the marker), not jump past it
    // into unrelated following content.
    #[test]
    fn hidden_closing_marker_boundary_lands_before_the_marker_not_after_it() {
        let text = "**bold** more";
        let mut editor = Editor::new(text);
        // The default caret sits at source offset 0, which is also where the
        // bold construct's own opening marker starts: `range_touches` treats
        // a caret exactly at a construct's start as touching it, which would
        // disclose (un-hide) the markers this test needs hidden. Move the
        // caret past the construct first, as the other cases below already do.
        editor
            .set_selection(Selection::caret(SourceOffset(text.len())))
            .unwrap();
        let lines = presented_lines(&editor);

        let line = &lines[0];
        assert_eq!(line.visual_text, "bold more");
        // The shared boundary between "bold" and " more" sits right where the
        // closing `**` collapsed to nothing: canonical is the end of "bold"
        // (source offset 6, just before the marker), not the start of " more"
        // (source offset 8, just after it).
        assert_eq!(
            source_offset_for_visual_position(&editor, 0, line, "bold".len(), None),
            SourceOffset(6)
        );
    }

    // Root cause behind the current PR #144 blocker: `offset_at_row_x` knew
    // which row (fragment) a click landed on, but discarded it before calling
    // `source_offset_for_visual_position`, so a soft-wrap boundary that shares
    // a visual offset with a hidden closing marker always resolved to the
    // marker's own edge (`Bias::Before`) regardless of which row was clicked.
    // A click at the very start of the row that begins after the wrap must
    // land in that row's own visible content, not back before the marker on
    // the row above. This fixes both sides of the same collapsed boundary as
    // `hidden_closing_marker_boundary_lands_before_the_marker_not_after_it`
    // above, which pins the no-wrap (whole-line fragment) case.
    #[test]
    fn soft_wrap_boundary_at_a_collapsed_marker_resolves_by_the_clicked_row() {
        let text = "**bold** more";
        let mut editor = Editor::new(text);
        editor
            .set_selection(Selection::caret(SourceOffset(text.len())))
            .unwrap();
        let lines = presented_lines(&editor);

        let line = &lines[0];
        assert_eq!(line.visual_text, "bold more");
        let boundary = "bold".len();

        // The upper row: it ends exactly at the collapsed closing marker, so
        // a click at its own trailing edge stays on the content it closes,
        // just before the marker (source offset 6).
        let upper_row = 0..boundary;
        assert_eq!(
            source_offset_for_visual_position(&editor, 0, line, boundary, Some(&upper_row)),
            SourceOffset(6)
        );

        // The next row: it starts exactly at the same collapsed point, so a
        // click at its own leading edge lands on the content it opens, just
        // after the marker (source offset 8), not back on the row above.
        let next_row = boundary..line.visual_text.len();
        assert_eq!(
            source_offset_for_visual_position(&editor, 0, line, boundary, Some(&next_row)),
            SourceOffset(8)
        );
    }

    #[test]
    fn layout_click_mapping_preserves_collapsed_marker_affinity() {
        let text = "**bold** more";
        let mut editor = Editor::new(text);
        editor
            .set_selection(Selection::caret(SourceOffset(text.len())))
            .unwrap();
        let index = BlockIndex::from_buffer(editor.document());
        let shaper = FixedAdvanceShaper::new(8.0);
        let (block, layout) = laid_out(&editor, &index, 0, &shaper);
        let line = &block.lines[0];
        let boundary = "bold".len();
        let x = boundary as f32 * 8.0;

        assert_eq!(
            layout.source_at_x_with_bias(&block, 0, x, &shaper, Bias::Before),
            Some(SourceOffset(6))
        );
        assert_eq!(
            layout.source_at_x_with_bias(&block, 0, x, &shaper, Bias::After),
            Some(SourceOffset(8))
        );
        assert_eq!(
            collapsed_boundary_bias(line, boundary, Some(&(0..boundary))),
            Bias::Before
        );
        assert_eq!(
            collapsed_boundary_bias(line, boundary, Some(&(boundary..line.visual_text.len()))),
            Bias::After
        );
    }

    #[test]
    fn hidden_closing_marker_boundary_lands_before_the_marker_across_a_joined_multiline_span() {
        // CommonMark resolves a backtick-delimited code span across a soft
        // line break, so the closing marker for `co` on the first physical
        // line lands on the second: `present_block` joins both lines' shared
        // parse the same way it would for a multiline bold or emphasis run.
        let editor = Editor::new("start `co\nde` end");
        let lines = presented_lines(&editor);

        let second = &lines[1];
        assert_eq!(second.visual_text, "de end");
        // The shared boundary between "de" and " end" sits where the closing
        // backtick collapsed to nothing: canonical is the end of "de" (source
        // offset 12, just before the marker), not the start of " end" (source
        // offset 13, just after it).
        assert_eq!(
            source_offset_for_visual_position(&editor, 1, second, "de".len(), None),
            SourceOffset(12)
        );
    }

    // Codex review on PR #144: a hidden *opening* marker (e.g. the leading
    // backtick of a code span) shares its collapsed visual position with the
    // end of whatever unrelated text precedes it, the mirror image of the
    // closing-marker case above. The canonical position for a click there
    // must land just after the marker, inside the content it opens, not just
    // before it in the unrelated preceding text.
    #[test]
    fn hidden_opening_marker_boundary_lands_after_the_marker_not_before_it() {
        let editor = Editor::new("this inline `code`");
        let lines = presented_lines(&editor);

        let line = &lines[0];
        assert_eq!(line.visual_text, "this inline code");
        // The shared boundary between "this inline " and "code" sits right
        // where the opening backtick collapsed to nothing: canonical is the
        // start of "code" (source offset 13, just after the marker), not the
        // end of "this inline " (source offset 12, just before it).
        assert_eq!(
            source_offset_for_visual_position(&editor, 0, line, "this inline ".len(), None),
            SourceOffset(13)
        );
    }

    // Mirrors the hosted GUI validator's `quote_open` probe
    // (`scripts/hosted_gui_interaction.py`): an opening marker nested inside a
    // blockquote container must resolve the same way a top-level one does.
    #[test]
    fn hidden_opening_marker_boundary_inside_a_quote_lands_after_the_marker() {
        let text = "> quote with **bold**";
        let editor = Editor::new(text);
        let lines = presented_lines(&editor);

        let line = &lines[0];
        assert_eq!(line.visual_text, "> quote with bold");
        let visual_offset = line.visual_text.find("bold").unwrap();
        // "**bold**" starts at source offset 13; the opening `**` ends at 15,
        // right where "bold" begins in the source.
        assert_eq!(
            source_offset_for_visual_position(&editor, 0, line, visual_offset, None),
            SourceOffset(text.find("**bold").unwrap() + 2)
        );
    }

    // Codex review on PR #144: an indented list item's bullet marker collapses
    // to zero visual width the same as any other hidden marker, but its
    // owning `ListItem` node's source range starts at the line's indentation
    // rather than at the bullet itself — so the naive start/end alignment
    // `marker_edge` otherwise uses to classify a delimiter never matches, and
    // the marker fell back to the closing-side default. That misclassified
    // the marker as closing content to its *left* (the indentation) instead
    // of opening the list item to its right, so a click at the start of the
    // visible text landed before the bullet instead of after it.
    #[test]
    fn hidden_list_marker_boundary_lands_after_the_marker_even_when_indented() {
        // A blank line closes the list before "next line", so the caret at
        // document end sits in an unrelated trailing paragraph rather than
        // (per CommonMark lazy continuation) inside the list item's own
        // source range, keeping the marker hidden for this boundary check.
        let text = "  - item\n\nnext line";
        let mut editor = Editor::new(text);
        editor
            .set_selection(Selection::caret(SourceOffset(text.len())))
            .unwrap();
        let lines = presented_lines(&editor);

        let line = &lines[0];
        assert_eq!(line.visual_text, "\u{2022} item");
        let visual_offset = line.visual_text.find("item").unwrap();
        // "  - item" hides the bullet `- ` (source offsets 2..4) and replaces
        // it with a synthesized `•`; canonical is source offset 4, just after
        // the marker and before "item", not offset 2, just before the marker
        // in the leading indentation.
        assert_eq!(
            source_offset_for_visual_position(&editor, 0, line, visual_offset, None),
            SourceOffset(text.find("- item").unwrap() + 2)
        );
    }

    // Codex review on PR #144: a standalone image with leading indentation
    // (`  ![alt](x)`) collapses the indentation's end, the hidden `![`, and
    // the start of the visible alt text to the same visual offset. Without a
    // marker edge the boundary defaulted to closing-side, landing a click
    // just before `![` in the indentation instead of just after it at the
    // start of the alt text.
    #[test]
    fn hidden_image_opening_marker_boundary_lands_after_the_marker() {
        let text = "  ![alt](x)\nnext line";
        let mut editor = Editor::new(text);
        editor
            .set_selection(Selection::caret(SourceOffset(text.len())))
            .unwrap();
        let lines = presented_lines(&editor);

        let line = &lines[0];
        assert_eq!(line.visual_text, "  alt");
        let visual_offset = line.visual_text.find("alt").unwrap();
        // "![alt](x)" starts at source offset 2; the opening `![` ends at 4,
        // right where "alt" begins in the source.
        assert_eq!(
            source_offset_for_visual_position(&editor, 0, line, visual_offset, None),
            SourceOffset(text.find("alt").unwrap())
        );
    }

    // Codex review on PR #144: an indented ATX heading (`  ## title`) collapses
    // the indentation's end and the start of the visible title to the same
    // visual offset, the same shape as the indented list/image cases above.
    // `Heading` was not in `has_delimiter_markers`, so `marker_edge` never
    // classified its opening marker and the boundary fell back to the
    // closing-side default, landing a click just before `## ` in the
    // indentation instead of just after it at the start of the title.
    #[test]
    fn hidden_heading_marker_boundary_lands_after_the_marker_even_when_indented() {
        let text = "  ## title\nnext line";
        let mut editor = Editor::new(text);
        editor
            .set_selection(Selection::caret(SourceOffset(text.len())))
            .unwrap();
        let lines = presented_lines(&editor);

        let line = &lines[0];
        assert_eq!(line.visual_text, "  title");
        let visual_offset = line.visual_text.find("title").unwrap();
        // "  ## title" hides the marker `## ` (source offsets 2..5); canonical
        // is source offset 5, just after the marker and before "title", not
        // offset 2, just before the marker in the leading indentation.
        assert_eq!(
            source_offset_for_visual_position(&editor, 0, line, visual_offset, None),
            SourceOffset(text.find("title").unwrap())
        );
    }

    // Codex review on PR #144: two adjacent links with no visible gap between
    // them (`[a](x)[b](y)`) collapse the first link's closing marker and the
    // second link's opening marker onto the exact same visual point.
    // `find_map` picked whichever segment happened to be first in source
    // order at that point — the first link's closing marker — so a click at
    // the start of "b" landed at the end of "a" instead, and subsequent input
    // edited the first link rather than the second.
    #[test]
    fn adjacent_links_boundary_lands_after_the_second_links_opening_marker() {
        let text = "[a](x)[b](y)\nnext line";
        let mut editor = Editor::new(text);
        editor
            .set_selection(Selection::caret(SourceOffset(text.len())))
            .unwrap();
        let lines = presented_lines(&editor);

        let line = &lines[0];
        assert_eq!(line.visual_text, "ab");
        let visual_offset = line.visual_text.find('b').unwrap();
        // The boundary between "a" and "b" sits where the first link's
        // closing marker and the second link's opening marker both collapse:
        // canonical is source offset 7, the start of "b", not offset 2, the
        // end of "a".
        assert_eq!(
            source_offset_for_visual_position(&editor, 0, line, visual_offset, None),
            SourceOffset(text.find('b').unwrap())
        );
    }

    #[test]
    fn moving_down_through_forty_lines_scrolls_cursor_to_viewport_bottom() {
        let text = (1..=40)
            .map(|line| format!("line {line:02}"))
            .collect::<Vec<_>>()
            .join("\n");
        let mut editor = Editor::new(&text);

        for _ in 0..32 {
            editor
                .dispatch(EditorCommand::MoveDown { extend: false })
                .unwrap();
        }

        let line = editor
            .document()
            .line_for_offset(editor.selection().active)
            .unwrap();
        let heights = HeightIndex::new(std::iter::repeat_n(DEFAULT_THEME.line_height, 40));
        let viewport_height = 722.0;
        let cursor_top = heights.prefix_sum(line.0);
        let scroll_y =
            scroll_y_for_cursor(0.0, cursor_top, DEFAULT_THEME.line_height, viewport_height);

        assert_eq!(line, LineId(32));
        assert_eq!(scroll_y, 136.0);
        assert_eq!(content_top_for_scroll(scroll_y), -136.0);
        assert_eq!(cursor_top - scroll_y, 696.0);
    }

    // Issue #240: the caret's input-mode badge is drawn below its row, so
    // parking that row flush against the viewport's bottom edge (as the test
    // above does for the raw geometry) would clip the badge under the
    // viewport's `overflow_hidden`. `scroll_cursor_into_view` asks for
    // `CARET_MODE_BADGE_HEIGHT` of extra clearance to prevent that.
    #[test]
    fn moving_down_through_forty_lines_leaves_room_for_the_caret_mode_badge() {
        let text = (1..=40)
            .map(|line| format!("line {line:02}"))
            .collect::<Vec<_>>()
            .join("\n");
        let mut editor = Editor::new(&text);

        for _ in 0..32 {
            editor
                .dispatch(EditorCommand::MoveDown { extend: false })
                .unwrap();
        }

        let line = editor
            .document()
            .line_for_offset(editor.selection().active)
            .unwrap();
        let heights = HeightIndex::new(std::iter::repeat_n(DEFAULT_THEME.line_height, 40));
        let viewport_height = 722.0;
        let cursor_top = heights.prefix_sum(line.0);
        let scroll_y = scroll_y_for_cursor(
            0.0,
            cursor_top,
            DEFAULT_THEME.line_height + CARET_MODE_BADGE_HEIGHT,
            viewport_height,
        );

        let row_bottom_in_viewport = cursor_top - scroll_y + DEFAULT_THEME.line_height;
        assert!(
            row_bottom_in_viewport + CARET_MODE_BADGE_HEIGHT <= viewport_height,
            "badge would be clipped: row bottom {row_bottom_in_viewport}, viewport {viewport_height}"
        );
    }

    // Issue #240 follow-up: the test above only exercises the pure
    // `scroll_y_for_cursor` math. In the real render path, the height-anchor
    // restore and the final `clamp_scroll_y` call both used to bound
    // `scroll_y` to the bare `self.heights.total_height()`, which does not
    // include the badge's footprint past the last line, so an actual render
    // pass at the document's end clamped the clearance away again and
    // clipped the badge under the viewport's `overflow_hidden`.
    #[gpui::test]
    fn moving_to_document_end_leaves_room_for_the_caret_mode_badge_after_render(
        cx: &mut gpui::TestAppContext,
    ) {
        let text = (1..=200)
            .map(|line| format!("line {line:02}"))
            .collect::<Vec<_>>()
            .join("\n\n");
        let (view, cx, _root) = open_view_for_mouse_tests(cx, &text, false);

        view.update(cx, |view, cx| {
            view.dispatch(EditorCommand::MoveToEnd { extend: false }, cx);
        });
        cx.run_until_parked();

        let (caret, viewport_height) = view.read_with(cx, |view, _| {
            (view.caret_geometry(), view.viewport_height)
        });
        let caret = caret.expect("caret is on screen at the document end");

        assert!(
            caret.y + caret.height + CARET_MODE_BADGE_HEIGHT <= viewport_height,
            "badge would be clipped: caret bottom {}, viewport {viewport_height}",
            caret.y + caret.height
        );
    }

    #[gpui::test]
    fn moving_into_a_hidden_closing_fence_rechecks_scroll_after_height_expands(
        cx: &mut gpui::TestAppContext,
    ) {
        let mut text = (1..=80)
            .map(|line| format!("line {line:02}"))
            .collect::<Vec<_>>()
            .join("\n\n");
        text.push_str("\n\n```\ncode\n```");
        let code_offset = text.find("\ncode\n").expect("code line") + 2;
        let closing_offset = text.rfind("```").expect("closing fence") + 1;
        let (view, cx, _root) = open_view_for_mouse_tests(cx, &text, false);

        view.update(cx, |view, cx| {
            view.editor_mut()
                .set_selection(Selection::caret(SourceOffset(code_offset)))
                .unwrap();
            view.after_input(cx);
        });
        cx.run_until_parked();

        view.update(cx, |view, cx| {
            view.dispatch(EditorCommand::MoveDown { extend: false }, cx);
        });
        cx.run_until_parked();

        let (active, caret, viewport_height) = view.read_with(cx, |view, _| {
            (
                view.editor().selection().active,
                view.caret_geometry(),
                view.viewport_height,
            )
        });
        assert_eq!(active, SourceOffset(closing_offset));
        let caret = caret.expect("disclosed closing fence caret is visible");
        assert!(caret.height > 0.0, "editing restores the fence row height");
        assert!(
            caret.y + caret.height + CARET_MODE_BADGE_HEIGHT
                <= viewport_height + CARET_VISIBILITY_TOLERANCE,
            "expanded fence caret would be clipped: bottom {}, viewport {viewport_height}",
            caret.y + caret.height
        );
    }

    #[gpui::test]
    fn moving_away_from_a_hidden_fence_shrinks_the_previous_height(
        cx: &mut gpui::TestAppContext,
    ) {
        let text = "```\nbody\n```\n\n```\n```";
        let view = gpui::AppContext::new(cx, |cx| EditorView::new(text, "Untitled", cx));
        let later_fence = text.rfind("```").expect("closing fence") + 1;

        view.update(cx, |view, cx| {
            let document = view.editor().document().clone();
            let index = BlockIndex::from_buffer(&document);
            view.block_index
                .publish(index, IndexSource::Formal, &document);
            view.editor_mut()
                .set_selection(Selection::caret(SourceOffset(1)))
                .unwrap();
            let inactive = block_heights_with_disclosure(
                &document,
                view.current_index().expect("formal index"),
                view.line_height(),
                None,
            );
            let active = block_heights_with_disclosure(
                &document,
                view.current_index().expect("formal index"),
                view.line_height(),
                Some(SourceRange::empty(1)),
            );
            view.install_heights(Granularity::Blocks, HeightIndex::new(active));
            let measured = code_line_height(view.line_height()) * 3.0;
            view.heights.update(0, measured);
            assert!(measured > inactive[0]);

            view.editor_mut()
                .set_selection(Selection::caret(SourceOffset(later_fence)))
                .unwrap();
            view.after_input(cx);

            let expected = code_line_height(view.line_height()) * 2.0;
            assert!(
                view.heights
                    .height(0)
                    .is_some_and(|height| (height - expected).abs() < 0.001),
                "leaving the old fence must remove only its disclosed row from the measured block"
            );
        });
    }
    #[test]
    fn height_remeasurement_cannot_leave_scroll_position_below_new_bottom() {
        // This is the position retained by the height anchor after a block at
        // the old bottom shrinks from 1,000px to 600px.
        assert_eq!(clamp_scroll_y(600.0, 600.0, 300.0), 300.0);
    }

    // Issue #228: continuous 50%-300% zoom.
    #[test]
    fn clamp_and_snap_zoom_clamps_to_the_supported_range_and_snaps_the_100_percent_band() {
        assert_eq!(clamp_and_snap_zoom(0.1), MIN_ZOOM);
        assert_eq!(clamp_and_snap_zoom(MIN_ZOOM), MIN_ZOOM);
        assert_eq!(clamp_and_snap_zoom(10.0), MAX_ZOOM);
        assert_eq!(clamp_and_snap_zoom(MAX_ZOOM), MAX_ZOOM);

        // The band around 100% snaps to exactly 1.0, at both edges...
        assert_eq!(clamp_and_snap_zoom(0.98), 1.0);
        assert_eq!(clamp_and_snap_zoom(1.0), 1.0);
        assert_eq!(clamp_and_snap_zoom(1.02), 1.0);
        // ...but a step past either edge is a real, un-snapped level, so a
        // continuing gesture always breaks free of the snap.
        assert_eq!(clamp_and_snap_zoom(0.97), 0.97);
        assert_eq!(clamp_and_snap_zoom(1.03), 1.03);
    }

    #[test]
    fn background_height_snapshot_must_match_the_current_line_height() {
        assert!(height_snapshot_matches_line_height(26.0, 26.0));
        assert!(!height_snapshot_matches_line_height(52.0, 26.0));
    }

    #[test]
    fn zoom_factor_for_wheel_zooms_in_toward_the_document_start_and_out_the_other_way() {
        let line_height = 26.0;
        // Positive `Lines` delta is the same direction `on_scroll` subtracts
        // from `scroll_y` to move toward the document start, so it must zoom
        // in; the opposite delta must zoom out by the exact inverse factor.
        let zoom_in = zoom_factor_for_wheel(ScrollDelta::Lines(point(0.0, 1.0)), line_height);
        let zoom_out = zoom_factor_for_wheel(ScrollDelta::Lines(point(0.0, -1.0)), line_height);
        assert!(zoom_in > 1.0, "{zoom_in}");
        assert!(zoom_out < 1.0, "{zoom_out}");
        assert!((zoom_in * zoom_out - 1.0).abs() < 1e-6);
    }

    #[test]
    fn zoom_factor_for_wheel_treats_pixel_deltas_as_lines_scaled_by_line_height() {
        let line_height = 26.0;
        let one_line_in_lines =
            zoom_factor_for_wheel(ScrollDelta::Lines(point(0.0, 1.0)), line_height);
        let one_line_in_pixels = zoom_factor_for_wheel(
            ScrollDelta::Pixels(point(px(0.0), px(line_height))),
            line_height,
        );
        assert!((one_line_in_lines - one_line_in_pixels).abs() < 1e-4);
    }

    #[gpui::test]
    fn set_zoom_clamps_snaps_and_reset_zoom_restores_100_percent(cx: &mut gpui::TestAppContext) {
        let view = gpui::AppContext::new(cx, |cx| EditorView::new("", "Untitled", cx));

        view.update(cx, |view, cx| {
            view.set_zoom(1.5, 0.0, cx);
            assert_eq!(view.zoom, 1.5);

            // Out-of-range levels clamp instead of overshooting.
            view.set_zoom(50.0, 0.0, cx);
            assert_eq!(view.zoom, MAX_ZOOM);
            view.set_zoom(-1.0, 0.0, cx);
            assert_eq!(view.zoom, MIN_ZOOM);

            // A level inside the 100% snap band lands on exactly 1.0, and
            // `reset_zoom` (the Ctrl/Cmd+0 action) returns there from any
            // other level too.
            view.set_zoom(1.01, 0.0, cx);
            assert_eq!(view.zoom, 1.0);
            view.set_zoom(2.0, 0.0, cx);
            assert_eq!(view.zoom, 2.0);
            view.reset_zoom(cx);
            assert_eq!(view.zoom, 1.0);
        });
    }

    #[gpui::test]
    fn scroll_y_for_zoom_anchor_keeps_the_anchored_point_at_the_same_window_offset(
        cx: &mut gpui::TestAppContext,
    ) {
        let view = gpui::AppContext::new(cx, |cx| EditorView::new("", "Untitled", cx));

        view.update(cx, |view, _cx| {
            // Ten 26px items; the viewport is scrolled so item 5 (top at
            // 130px) sits 20px above the middle of the window.
            view.heights = HeightIndex::new(std::iter::repeat_n(26.0, 10));
            view.scroll_y = 100.0;
            let window_offset = 50.0;

            let anchor = view
                .zoom_anchor_at(window_offset)
                .expect("heights are populated");
            assert_eq!(anchor.ordinal, 5);
            assert!((anchor.fraction - 20.0 / 26.0).abs() < 1e-6);

            // Zooming to 200% doubles every item's height; the same ordinal
            // and fraction must resolve to a `scroll_y` that keeps the
            // anchor at the same `window_offset`.
            view.heights = HeightIndex::new(std::iter::repeat_n(52.0, 10));
            let scroll_y = view.scroll_y_for_zoom_anchor(anchor);
            let content_y = view.heights.prefix_sum(anchor.ordinal)
                + anchor.fraction * view.heights.height(anchor.ordinal).unwrap();
            assert!((content_y - (scroll_y + window_offset)).abs() < 1e-3);
        });
    }

    #[test]
    fn cache_invalidation_only_marks_intersecting_lines() {
        let range = SourceRange::new(10, 20);
        let before = hane_document::Revision(1);
        let after = hane_document::Revision(2);
        let inside = RevisionDelta {
            from_revision: before,
            to_revision: after,
            edited_source_range_before: SourceRange::empty(15),
            edited_source_range_after: SourceRange::new(15, 16),
            byte_delta: 1,
        };
        let before_line = RevisionDelta {
            edited_source_range_before: SourceRange::empty(3),
            edited_source_range_after: SourceRange::new(3, 4),
            ..inside
        };
        assert!(edit_affects_range(inside, range));
        assert!(!edit_affects_range(before_line, range));
    }

    #[test]
    fn local_fallback_resolves_the_fenced_block_before_a_formal_index_exists() {
        let editor = Editor::new("before\n```rust\nlet answer = 42;\n```\nafter\n");
        let document = editor.document();
        let local = local_block_index(document, 0..6);
        let kind = |line: usize| {
            local
                .block_at(document.line_range(LineId(line)).unwrap().start)
                .map(|block| block.kind)
        };
        assert_eq!(kind(0), Some(hane_markdown::NodeKind::Paragraph));
        for inside in 1..=3 {
            assert_eq!(kind(inside), Some(hane_markdown::NodeKind::CodeBlock));
        }
        assert_eq!(kind(4), Some(hane_markdown::NodeKind::Paragraph));
    }

    #[test]
    fn a_block_taller_than_the_viewport_presents_only_the_visible_lines() {
        // No blank line anywhere, so CommonMark reads the whole document as a
        // single paragraph: the block is the document, and clipping is the only
        // thing keeping element generation bounded.
        let editor = Editor::new(&"短い段落です。\n".repeat(100_000));
        let index = BlockIndex::from_buffer(editor.document());
        assert_eq!(index.len(), 1, "the whole document is one block");
        let lines = editor.document().line_count();

        let block = index.block(0).unwrap();
        let visual = presented_block(&editor, &block, &(40_000..40_050), None).unwrap();
        assert_eq!(visual.lines.len(), 50, "only the visible lines are built");
        assert_eq!(visual.lines_before, 40_000);
        assert_eq!(visual.lines_after, lines - 40_050);
        // The clipped lines still account for their height, so the scroll range
        // does not depend on what has been drawn.
        assert_eq!(visual.height(), lines as f32 * 26.0);
        assert_eq!(visual.leading_space(), 40_000.0 * 26.0);
        assert!(visual.covers(&(40_010..40_040)));
        assert!(!visual.covers(&(39_000..39_050)));
    }

    #[test]
    fn visible_line_prefix_inversion_skips_a_long_collapsed_fence_prefix() {
        // Model a large list/quote block whose first 5,000 physical rows are
        // inactive fence delimiters. The inverse must select the first visible
        // source row, not the physical row with the same ordinal as the visual
        // y position.
        const COLLAPSED: usize = 5_000;
        const PHYSICAL: usize = 10_000;
        let visible_prefix = |physical: usize| {
            physical.saturating_sub(physical.min(COLLAPSED))
        };

        assert_eq!(
            invert_visible_line_prefix(PHYSICAL, 1, visible_prefix),
            COLLAPSED + 1
        );
        assert_eq!(
            invert_visible_line_prefix(PHYSICAL, 37, visible_prefix),
            COLLAPSED + 37
        );
        assert_eq!(
            invert_visible_line_prefix(PHYSICAL, PHYSICAL, visible_prefix),
            PHYSICAL
        );
    }

    #[gpui::test]
    fn clipped_nested_fence_window_uses_the_non_collapsed_source_prefix(
        cx: &mut gpui::TestAppContext,
    ) {
        let mut text = String::new();
        for _ in 0..128 {
            text.push_str("> ```\n> ```\n> \n");
        }
        text.push_str("> visible tail\n\noutside");
        let view = gpui::AppContext::new(cx, |cx| EditorView::new(&text, "Untitled", cx));

        view.update(cx, |view, _cx| {
            view.editor_mut()
                .set_selection(Selection::caret(SourceOffset(text.len())))
                .unwrap();
            let document = view.editor().document().clone();
            let index = BlockIndex::from_buffer(&document);
            let block = index.blocks().next().expect("quote block");
            assert!(
                index.fence_height_projection(&block).is_some(),
                "fixture must build a nested fence height projection"
            );
            view.block_index.publish(index, IndexSource::Formal, &document);
            let block = view.current_index().unwrap().block(0).unwrap();
            let span = block_line_span(&document, &block).unwrap();
            view.install_heights(
                Granularity::Blocks,
                HeightIndex::new(block_heights_with_disclosure(
                    &document,
                    view.current_index().unwrap(),
                    view.line_height(),
                    None,
                )),
            );
            view.viewport_height = view.line_height() * 4.0;
            view.scroll_y = view.line_height() * 50.0;

            let lines = view.visible_line_window(&[block], &(0..1));
            assert!(
                lines.start > span.start + 80,
                "collapsed fence rows must be skipped when selecting the source window: {lines:?}"
            );
            assert!(lines.end > lines.start && lines.end <= span.end);

            let source_anchor = document.line_range(LineId(120)).unwrap().start;
            let anchored_y = view.scroll_for_offset(source_anchor);
            assert_eq!(
                anchored_y,
                view.line_height() * 40.0,
                "scroll anchors must use the same collapsed-row prefix as the render window"
            );
        });
    }

    #[test]
    fn a_large_list_viewport_keeps_formal_numbering_and_nesting() {
        let mut source = String::from("1. outer\n   1. nested\n1. second\n");
        for _ in 3..5_000 {
            source.push_str("1. item\n");
        }
        let editor = Editor::new(&source);
        let index = BlockIndex::from_buffer(editor.document());
        let block = index.blocks().next().expect("one list block");
        let projection = index.list_projection(&block).expect("list projection");

        // The nested item is rendered from a one-line viewport parse. Its
        // parent list is outside that parse, so the formal depth must come
        // from the projection retained by BlockIndex.
        let nested = presented_block_with_list_projection(
            &editor,
            &block,
            &(1..2),
            None,
            Some(projection),
            DEFAULT_LINE_HEIGHT,
        )
        .expect("nested item presents");
        assert_eq!(nested.lines[0].visual_text, "1. nested");
        assert_eq!(nested.lines[0].list.as_ref().unwrap().owner.depth, 2);

        // The late item is past the synchronous 4,096-line join budget. Its
        // source marker is still `1.`, but the inactive presentation must use
        // the direct-child ordinal from the whole list rather than restarting
        // at `1.` in the viewport.
        let late_line = 3_000;
        let late = presented_block_with_list_projection(
            &editor,
            &block,
            &(late_line..late_line + 1),
            None,
            Some(projection),
            DEFAULT_LINE_HEIGHT,
        )
        .expect("late item presents");
        assert_eq!(late.lines[0].visual_text, "3000. item");
        assert_eq!(late.lines[0].list.as_ref().unwrap().owner.ordinal, 2_999);
    }

    #[test]
    fn a_late_list_continuation_keeps_formal_owner_and_structural_indent() {
        let mut source = String::from("10. opening\n");
        for _ in 0..5_000 {
            source.push_str("    continued\n");
        }
        let editor = Editor::new(&source);
        let index = BlockIndex::from_buffer(editor.document());
        let block = index.blocks().next().expect("one list block");
        let projection = index.list_projection(&block).expect("list projection");

        // The opening marker is outside the synchronous viewport parse. The
        // formal projection must still identify this row as the first item and
        // hide its structural four-column continuation prefix.
        let late_line = 4_500;
        let late = presented_block_with_list_projection(
            &editor,
            &block,
            &(late_line..late_line + 1),
            None,
            Some(projection),
            DEFAULT_LINE_HEIGHT,
        )
        .expect("late continuation presents");
        let line = &late.lines[0];
        assert_eq!(line.visual_text, "continued");
        let list = line.list.as_ref().expect("formal list row metadata");
        assert_eq!(list.owner.ordinal, 0);
        assert_eq!(list.owner.depth, 1);
        assert_eq!(list.structural_prefixes.len(), 1);
        assert_eq!(list.structural_prefixes[0].columns, 4);
        assert!(line.style_runs.is_empty(), "continuation is not code");
    }

    #[test]
    fn a_late_list_code_row_keeps_formal_code_display() {
        let mut source = String::from("- opening\n");
        for _ in 0..5_000 {
            source.push_str("  continued\n");
        }
        source.push_str("\n  ```rust\n  **literal**\n  ```\n");
        let editor = Editor::new(&source);
        let index = BlockIndex::from_buffer(editor.document());
        let block = index.blocks().next().expect("one list block");
        let projection = index.list_projection(&block).expect("list projection");
        let code_line = source[..source.find("  **literal**").expect("code row")]
            .bytes()
            .filter(|byte| *byte == b'\n')
            .count();

        let code = presented_block_with_list_projection(
            &editor,
            &block,
            &(code_line..code_line + 1),
            None,
            Some(projection),
            DEFAULT_LINE_HEIGHT,
        )
        .expect("late code row presents");
        let line = &code.lines[0];
        assert_eq!(line.visual_text, "**literal**");
        assert_eq!(line.kind, BlockKind::CodeBlock);
        assert!(
            line.style_runs
                .iter()
                .any(|run| run.kind == StyleKind::CodeBlock)
        );
        assert_eq!(line.style_runs.len(), 1);
        assert_eq!(line.style_runs[0].kind, StyleKind::CodeBlock);

        let opening_line = code_line - 1;
        let opening = presented_block_with_list_projection(
            &editor,
            &block,
            &(opening_line..opening_line + 1),
            None,
            Some(projection),
            DEFAULT_LINE_HEIGHT,
        )
        .expect("late code opening presents");
        assert_eq!(opening.lines[0].visual_text, "rust");
        assert!(opening.lines[0].source_map.segments.iter().any(|segment| {
            segment.visibility == Visibility::HiddenMarkup
                && segment.source_range.end.0 > segment.source_range.start.0
        }));
    }

    #[test]
    fn a_late_list_code_row_keeps_literal_shorter_fence_visible() {
        let mut source = String::from("- opening\n  ````rust\n");
        for _ in 0..5_000 {
            source.push_str("  literal\n");
        }
        source.push_str("  ```oops\n  ````\n");
        let editor = Editor::new(&source);
        let index = BlockIndex::from_buffer(editor.document());
        let block = index.blocks().next().expect("one list block");
        let projection = index.list_projection(&block).expect("list projection");
        let lookalike_line = source[..source.find("  ```oops").expect("lookalike row")]
            .bytes()
            .filter(|byte| *byte == b'\n')
            .count();

        let presented = presented_block_with_list_projection(
            &editor,
            &block,
            &(lookalike_line..lookalike_line + 1),
            None,
            Some(projection),
            DEFAULT_LINE_HEIGHT,
        )
        .expect("late code row presents");
        let line = &presented.lines[0];
        assert_eq!(line.visual_text, "```oops");
        assert!(line.style_runs.iter().any(|run| run.kind == StyleKind::CodeBlock));
    }

    #[test]
    fn a_late_nested_list_fence_uses_formal_markers_when_viewport_parse_misses_it() {
        let mut source = String::from("- outer\n  - nested\n    ````rust\n");
        for _ in 0..5_000 {
            source.push_str("    literal\n");
        }
        source.push_str("    ```oops\n    ````");
        let editor = Editor::new(&source);
        let index = BlockIndex::from_buffer(editor.document());
        let block = index.blocks().next().expect("one nested list block");
        let projection = index.list_projection(&block).expect("list projection");
        let lookalike_line = source[..source.find("    ```oops").expect("lookalike row")]
            .bytes()
            .filter(|byte| *byte == b'\n')
            .count();

        let presented = presented_block_with_list_projection(
            &editor,
            &block,
            &(lookalike_line..lookalike_line + 1),
            None,
            Some(projection),
            DEFAULT_LINE_HEIGHT,
        )
        .expect("late nested code row presents");
        assert_eq!(presented.lines[0].visual_text, "```oops");
        assert!(presented.lines[0]
            .style_runs
            .iter()
            .any(|run| run.kind == StyleKind::CodeBlock));

        let closing_line = source[..source.rfind("    ````").expect("closing fence")]
            .bytes()
            .filter(|byte| *byte == b'\n')
            .count();
        let closing = presented_block_with_list_projection(
            &editor,
            &block,
            &(closing_line..closing_line + 1),
            None,
            Some(projection),
            DEFAULT_LINE_HEIGHT,
        )
        .expect("late nested closing fence presents");
        assert_eq!(closing.lines[0].visual_text, "");
        assert_eq!(
            closing.lines[0].source_map.segments.last().unwrap().marker_edge,
            Some(MarkerEdge::Closing)
        );
    }

    #[test]
    fn a_late_quote_code_row_uses_formal_code_projection() {
        let mut source = String::from("> ````rust\n");
        for _ in 0..5_000 {
            source.push_str("> > **literal**\n");
        }
        source.push_str("> ````");
        let editor = Editor::new(&source);
        let index = BlockIndex::from_buffer(editor.document());
        let block = index.blocks().next().expect("one quote block");
        let projection = index
            .list_projection(&block)
            .expect("formal quote code projection");
        let code_line = source[..source.find("> > **literal**").expect("code row")]
            .bytes()
            .filter(|byte| *byte == b'\n')
            .count();

        let code = presented_block_with_list_projection(
            &editor,
            &block,
            &(code_line..code_line + 1),
            None,
            Some(projection),
            DEFAULT_LINE_HEIGHT,
        )
        .expect("late quote code row presents");
        let line = &code.lines[0];
        assert_eq!(line.visual_text, "> **literal**");
        assert_eq!(line.kind, BlockKind::CodeBlock);
        assert!(line
            .style_runs
            .iter()
            .any(|run| run.kind == StyleKind::CodeBlock));

        let opening = presented_block_with_list_projection(
            &editor,
            &block,
            &(code_line - 1..code_line),
            None,
            Some(projection),
            DEFAULT_LINE_HEIGHT,
        )
        .expect("late quote code opening presents");
        assert_eq!(opening.lines[0].visual_text, "> rust");
        assert!(opening.lines[0].source_map.segments.iter().any(|segment| {
            segment.visibility == Visibility::HiddenMarkup
                && segment.marker_edge == Some(MarkerEdge::Opening)
        }));
    }

    #[test]
    fn a_late_list_quote_code_row_keeps_the_formal_quote_owner() {
        let mut source = String::from("- outer\n    > ````rust\n");
        for _ in 0..5_000 {
            source.push_str("    > > literal\n");
        }
        source.push_str("    > ````");
        let mut editor = Editor::new(&source);
        let index = BlockIndex::from_buffer(editor.document());
        let block = index.blocks().next().expect("one list block");
        let projection = index
            .list_projection(&block)
            .expect("formal list quote code projection");
        editor
            .set_selection(Selection::caret(SourceOffset(
                source.find("literal").expect("code content"),
            )))
            .unwrap();
        let code_line = source[..source.find("    > > literal").expect("code row")]
            .bytes()
            .filter(|byte| *byte == b'\n')
            .count();

        let code = presented_block_with_list_projection(
            &editor,
            &block,
            &(code_line..code_line + 1),
            None,
            Some(projection),
            DEFAULT_LINE_HEIGHT,
        )
        .expect("late list quote code row presents");
        assert_eq!(code.lines[0].visual_text, "    > > literal");
        assert_eq!(code.lines[0].kind, BlockKind::CodeBlock);
        let line_start = source.find("    > > literal").expect("code row source");
        let quote_marker = SourceRange::new(line_start + 4, line_start + 6);
        assert!(code.lines[0].source_map.segments.iter().any(|segment| {
            segment.source_range == quote_marker
                && segment.marker_edge == Some(MarkerEdge::Opening)
        }));
        assert!(code.lines[0].source_map.segments.iter().any(|segment| {
            segment.source_range == quote_marker
                && segment.visibility == Visibility::ExpandedMarkup
        }));
    }

    #[test]
    fn a_late_list_code_row_keeps_literal_list_marker_visible() {
        let mut source = String::from("- opening\n  ````rust\n");
        for _ in 0..5_000 {
            source.push_str("  - literal\n");
        }
        source.push_str("  ````");
        let editor = Editor::new(&source);
        let index = BlockIndex::from_buffer(editor.document());
        let block = index.blocks().next().expect("one list block");
        let projection = index.list_projection(&block).expect("list projection");
        let code_line = source[..source.find("  - literal").expect("code row")]
            .bytes()
            .filter(|byte| *byte == b'\n')
            .count();

        let code = presented_block_with_list_projection(
            &editor,
            &block,
            &(code_line..code_line + 1),
            None,
            Some(projection),
            DEFAULT_LINE_HEIGHT,
        )
        .expect("late list code row presents");
        let line = &code.lines[0];
        assert_eq!(line.visual_text, "- literal");
        assert_eq!(line.kind, BlockKind::CodeBlock);
        assert!(line
            .style_runs
            .iter()
            .any(|run| run.kind == StyleKind::CodeBlock));
    }

    #[test]
    fn a_same_line_nested_list_fence_keeps_the_formal_child_marker() {
        let mut source = String::from("- outer\n  - nested\n    - ````rust\n");
        for _ in 0..5_000 {
            source.push_str("      literal\n");
        }
        source.push_str("      ````");
        let editor = Editor::new(&source);
        let index = BlockIndex::from_buffer(editor.document());
        let block = index.blocks().next().expect("one nested list block");
        let projection = index.list_projection(&block).expect("list projection");
        let opening_line = source[..source.find("    - ````rust").expect("opening row")]
            .bytes()
            .filter(|byte| *byte == b'\n')
            .count();

        let opening = presented_block_with_list_projection(
            &editor,
            &block,
            &(opening_line..opening_line + 1),
            None,
            Some(projection),
            DEFAULT_LINE_HEIGHT,
        )
        .expect("same-line nested fence presents");
        let line = &opening.lines[0];
        assert_eq!(line.visual_text, "• rust");
        assert_eq!(line.kind, BlockKind::CodeBlock);
        let list = line.list.as_ref().expect("formal child list metadata");
        assert_eq!(list.role, ListRowRole::Opening);
        assert!(list.marker.as_ref().is_some_and(|marker| marker.synthesized));
    }

    #[test]
    fn a_same_line_multi_level_list_fence_uses_the_deepest_formal_marker() {
        let mut source = String::from("- outer\n    - - ````rust\n");
        for _ in 0..5_000 {
            source.push_str("          literal\n");
        }
        source.push_str("          ````");
        let mut editor = Editor::new(&source);
        let index = BlockIndex::from_buffer(editor.document());
        let block = index.blocks().next().expect("one nested list block");
        let projection = index.list_projection(&block).expect("list projection");
        let opening_start = source.find("    - - ````rust").expect("opening row");
        let opening_line = source[..opening_start]
            .bytes()
            .filter(|byte| *byte == b'\n')
            .count();

        let opening = presented_block_with_list_projection(
            &editor,
            &block,
            &(opening_line..opening_line + 1),
            None,
            Some(projection),
            DEFAULT_LINE_HEIGHT,
        )
        .expect("same-line multi-level fence presents");
        let line = &opening.lines[0];
        assert_eq!(line.visual_text, "• rust");
        assert_eq!(line.kind, BlockKind::CodeBlock);
        let list = line.list.as_ref().expect("formal child list metadata");
        assert_eq!(list.owner.depth, 3);
        assert_eq!(list.role, ListRowRole::Opening);
        let marker = list.marker.as_ref().expect("deepest marker metadata");
        assert!(marker.synthesized);
        assert_eq!(marker.source_range.start.0, opening_start + 6);

        editor
            .set_selection(Selection::caret(SourceOffset(
                opening_start + "    - - ````rust".find("rust").unwrap(),
            )))
            .unwrap();
        let disclosed = presented_block_with_list_projection(
            &editor,
            &block,
            &(opening_line..opening_line + 1),
            None,
            Some(projection),
            DEFAULT_LINE_HEIGHT,
        )
        .expect("disclosed same-line multi-level fence presents");
        let outer_marker = SourceRange::new(opening_start + 4, opening_start + 6);
        let inner_marker = SourceRange::new(opening_start + 6, opening_start + 8);
        for marker_range in [outer_marker, inner_marker] {
            assert!(disclosed.lines[0].source_map.segments.iter().any(|segment| {
                segment.source_range == marker_range
                    && segment.visibility == Visibility::ExpandedMarkup
            }));
        }
    }

    #[test]
    fn a_sibling_boundary_does_not_disclose_the_previous_formal_prefix() {
        let mut source = String::from("- first\n");
        for _ in 0..5_000 {
            source.push_str("  continued\n");
        }
        source.push_str("- second\n");
        let second_start = source.find("- second").expect("second item");
        let mut editor = Editor::new(&source);
        editor
            .set_selection(Selection::caret(SourceOffset(second_start)))
            .unwrap();
        let index = BlockIndex::from_buffer(editor.document());
        let block = index.blocks().next().expect("one list block");
        let projection = index.list_projection(&block).expect("list projection");
        let continuation_line = 5_000;

        let late = presented_block_with_list_projection(
            &editor,
            &block,
            &(continuation_line..continuation_line + 1),
            None,
            Some(projection),
            DEFAULT_LINE_HEIGHT,
        )
        .expect("formal continuation presents");
        assert_eq!(late.lines[0].visual_text, "continued");
    }

    #[test]
    fn one_block_covers_every_physical_line_of_its_construct() {
        let source = "# title\n\n```rust\nlet x = 1;\nlet y = 2;\n```\n\ntail\n";
        let editor = Editor::new(source);
        let index = BlockIndex::from_buffer(editor.document());
        let spans = index
            .blocks()
            .map(|block| block_line_span(editor.document(), &block).expect("block spans lines"))
            .collect::<Vec<_>>();

        // Three blocks for nine lines: virtualization is driven by the three,
        // not by the nine.
        assert_eq!(spans, vec![0..2, 2..7, 7..9]);
        assert_eq!(
            spans.last().map(|span| span.end),
            Some(editor.document().line_count()),
            "the empty final line a trailing newline creates is still drawn"
        );
        // Every line is drawn exactly once.
        assert_eq!(
            spans.iter().map(std::ops::Range::len).sum::<usize>(),
            editor.document().line_count()
        );
    }

    #[test]
    fn a_presented_block_carries_all_of_its_lines() {
        let editor = Editor::new("```rust\nlet x = 1;\nlet y = 2;\n```\n\ntail\n");
        let index = BlockIndex::from_buffer(editor.document());
        let code =
            presented_block(&editor, &index.block(0).unwrap(), &(0..usize::MAX), None).unwrap();
        assert_eq!(code.lines.len(), 5, "four fence lines plus the blank below");
        assert_eq!(code.source_range, index.block(0).unwrap().source_range);
        assert_eq!(
            code.height(),
            code.lines.iter().map(VisualLine::height).sum::<f32>()
        );
        assert!(
            code.matches(&index.block(0).unwrap()),
            "a freshly presented block matches the index it came from"
        );
    }

    #[gpui::test]
    fn byte_large_low_line_count_block_receives_background_shared_semantics(
        cx: &mut gpui::TestAppContext,
    ) {
        // Both delimiters are outside the small visible middle line. The byte
        // budget excludes synchronous full parsing, so only the real background
        // scheduler can provide the Strong semantics and invalidate the initial
        // bounded presentation.
        let padding = "x".repeat(crate::line::JOIN_SYNC_BYTE_BUDGET);
        let text = format!("before **{padding}\nmiddle\nend**\n");
        let view = gpui::AppContext::new(cx, |cx| EditorView::new(&text, "Untitled", cx));
        let indexed = view.update(cx, |view, cx| {
            let document = view.sessions.active().editor().document();
            view.block_index.publish(
                BlockIndex::from_buffer(document),
                IndexSource::Formal,
                document,
            );
            let indexed = view.block_at_offset(SourceOffset(0)).unwrap();
            let span = block_line_span(view.editor().document(), &indexed).unwrap();
            assert!(span.len() <= JOIN_SYNC_LINE_BUDGET);
            assert!(!block_fits_sync_join_budget(&indexed, &span));

            let (initial, _) = view.cached_block(&indexed, &(1..2)).unwrap();
            assert_eq!(initial.lines.len(), 1);
            assert!(
                initial.lines[0]
                    .style_runs
                    .iter()
                    .all(|run| run.kind != hane_presentation::StyleKind::Bold)
            );
            view.schedule_joined_parse(std::slice::from_ref(&indexed), cx);
            assert!(view.joined_parse_jobs.contains_key(&indexed.id));
            indexed
        });

        cx.run_until_parked();

        view.update(cx, |view, _cx| {
            assert!(!view.joined_parse_jobs.contains_key(&indexed.id));
            let cached = view.joined_parse_cache.get(&indexed.id).unwrap();
            assert_eq!(cached.revision, view.editor().document().revision());
            assert_eq!(cached.source_range, indexed.source_range);
            let (presented, reused) = view.cached_block(&indexed, &(1..2)).unwrap();
            assert!(
                !reused,
                "publishing shared semantics invalidates the fallback"
            );
            assert_eq!(presented.lines.len(), 1);
            assert!(
                presented.lines[0]
                    .style_runs
                    .iter()
                    .any(|run| run.kind == hane_presentation::StyleKind::Bold)
            );
        });
    }

    #[gpui::test]
    fn background_formal_parse_keeps_the_caret_owned_fence_block_visible(
        cx: &mut gpui::TestAppContext,
    ) {
        // Empty fenced blocks collapse to zero height when inactive. Keep
        // several of them adjacent so a disclosure-less background snapshot
        // would make the height index's y=0 lookup select a later block and
        // drop the caret-owned first block from virtualization.
        let text = (0..8)
            .map(|_| "```\n```")
            .collect::<Vec<_>>()
            .join("\n\n");
        let (view, cx, _root) = open_view_for_mouse_tests(cx, &text, false);

        // `schedule_document_parse` deliberately debounces formal work by a
        // short real timer; let that job publish before inspecting the layout.
        std::thread::sleep(Duration::from_millis(100));
        cx.run_until_parked();

        view.read_with(cx, |view, _| {
            assert_eq!(view.granularity, Granularity::Blocks);
            assert!(view.current_index().is_some_and(|index| index.len() >= 8));
            let first_height = view.heights.height(0).expect("first block height");
            assert!(
                first_height > 0.0,
                "the caret-owned opening fence must remain addressable after formal parse"
            );
            assert!(
                view.caret_geometry().is_some(),
                "formal height publication must not virtualize away the caret"
            );
        });
    }

    #[gpui::test]
    fn background_formal_parse_rebuilds_heights_when_disclosure_moves_during_parse(
        cx: &mut gpui::TestAppContext,
    ) {
        let text = (0..8)
            .map(|_| "```\n```")
            .collect::<Vec<_>>()
            .join("\n\n");
        let view = gpui::AppContext::new(cx, |cx| EditorView::new(&text, "Untitled", cx));
        let later_fence = text.rfind("```").expect("last fence");
        view.update(cx, |view, cx| {
            let document = view.editor().document().clone();
            let index = BlockIndex::from_buffer(&document);
            view.block_index
                .publish(index, IndexSource::Provisional, &document);
            let heights = HeightIndex::new(block_heights_with_disclosure(
                &document,
                view.current_index().unwrap(),
                view.line_height(),
                Some(SourceRange::empty(0)),
            ));
            view.install_heights(Granularity::Blocks, heights);
            // Start a formal parse with the old disclosure, then move the
            // caret before its completion without changing the document.
            view.schedule_document_parse(cx);
            view.editor_mut()
                .set_selection(Selection::caret(SourceOffset(later_fence)))
                .unwrap();
        });

        cx.run_until_parked();
        std::thread::sleep(Duration::from_millis(100));
        cx.run_until_parked();

        view.read_with(cx, |view, _| {
            let last = view.current_index().unwrap().len() - 1;
            assert!(
                view.heights.height(last).is_some_and(|height| height > 0.0),
                "the current caret disclosure must win over the parse snapshot"
            );
        });
    }

    #[gpui::test]
    fn background_selection_height_snapshot_covers_the_selected_fence_range(
        cx: &mut gpui::TestAppContext,
    ) {
        let text = (0..16)
            .map(|_| "```\n```")
            .collect::<Vec<_>>()
            .join("\n\n");
        let view = gpui::AppContext::new(cx, |cx| EditorView::new(&text, "Untitled", cx));

        let inactive_middle_height = view.update(cx, |view, cx| {
            let document = view.editor().document().clone();
            let index = BlockIndex::from_buffer(&document);
            view.block_index
                .publish(index, IndexSource::Formal, &document);
            let index = view.current_index().expect("formal index");
            view.install_heights(
                Granularity::Blocks,
                HeightIndex::new(block_heights_with_disclosure(
                    &document,
                    index,
                    view.line_height(),
                    None,
                )),
            );
            let inactive_middle_height = view.heights.height(8).expect("middle block height");
            view.editor_mut()
                .set_selection(Selection {
                    anchor: SourceOffset(0),
                    active: SourceOffset(text.len()),
                })
                .unwrap();
            view.after_input(cx);
            assert!(
                view.document_parse_job_running,
                "a non-empty selection should refresh heights off the input path"
            );
            assert_eq!(
                view.heights.height(8),
                Some(inactive_middle_height),
                "the middle block must not be synchronously scanned"
            );
            inactive_middle_height
        });

        // Register the timer before sleeping so the test executor can observe
        // its wakeup, matching the formal-parse regression tests above.
        cx.run_until_parked();
        std::thread::sleep(Duration::from_millis(100));
        cx.run_until_parked();

        view.read_with(cx, |view, _| {
            assert!(
                view.heights
                    .height(8)
                    .is_some_and(|height| height > inactive_middle_height),
                "the background disclosure snapshot must expand selected fence blocks"
            );
        });
    }

    #[gpui::test]
    fn background_selection_height_snapshot_collapses_middle_blocks_when_selection_ends(
        cx: &mut gpui::TestAppContext,
    ) {
        let text = (0..16)
            .map(|_| "```\n```")
            .collect::<Vec<_>>()
            .join("\n\n");
        let view = gpui::AppContext::new(cx, |cx| EditorView::new(&text, "Untitled", cx));

        let inactive_middle_height = view.update(cx, |view, cx| {
            let document = view.editor().document().clone();
            let index = BlockIndex::from_buffer(&document);
            view.block_index
                .publish(index, IndexSource::Formal, &document);
            let index = view.current_index().expect("formal index");
            view.install_heights(
                Granularity::Blocks,
                HeightIndex::new(block_heights_with_disclosure(
                    &document,
                    index,
                    view.line_height(),
                    None,
                )),
            );
            let inactive_middle_height = view.heights.height(8).expect("middle block height");
            view.editor_mut()
                .set_selection(Selection {
                    anchor: SourceOffset(0),
                    active: SourceOffset(text.len()),
                })
                .unwrap();
            view.after_input(cx);
            inactive_middle_height
        });

        cx.run_until_parked();
        std::thread::sleep(Duration::from_millis(100));
        cx.run_until_parked();

        let expanded_middle_height = view.read_with(cx, |view, _| {
            let height = view.heights.height(8).expect("selected middle block height");
            assert!(height > inactive_middle_height);
            height
        });

        view.update(cx, |view, cx| {
            view.editor_mut()
                .set_selection(Selection::caret(SourceOffset(0)))
                .unwrap();
            view.after_input(cx);
            assert!(
                view.document_parse_job_running,
                "collapsing a selection must refresh heights off the input path"
            );
            assert_eq!(
                view.heights.height(8),
                Some(expanded_middle_height),
                "the middle block must not be synchronously scanned on caret movement"
            );
        });

        cx.run_until_parked();
        std::thread::sleep(Duration::from_millis(100));
        cx.run_until_parked();

        view.read_with(cx, |view, _| {
            assert_eq!(
                view.heights.height(8),
                Some(inactive_middle_height),
                "the empty disclosure snapshot must collapse the old selection interior"
            );
            assert!(
                view.heights.height(0).is_some_and(|height| height > 0.0),
                "the caret-owned fence block must remain addressable"
            );
        });
    }

    #[gpui::test]
    fn joined_parse_work_is_bounded_across_scrolling_and_document_switches(
        cx: &mut gpui::TestAppContext,
    ) {
        for switch_document in [false, true] {
            let paragraph = "x".repeat(crate::line::JOIN_SYNC_BYTE_BUDGET + 1);
            let text = (0..8)
                .map(|i| format!("{i} **{paragraph}\nend**\n\n"))
                .collect::<String>();
            let view = gpui::AppContext::new(cx, |cx| EditorView::new(&text, "First", cx));
            let current = view.update(cx, |view, cx| {
                let index = BlockIndex::from_buffer(view.editor().document());
                // Rapid viewport changes before background completions must not
                // accumulate a parse of every paragraph we passed through.
                for ordinal in 0..8 {
                    view.schedule_joined_parse(&[index.block(ordinal).unwrap()], cx);
                    assert!(view.joined_parse_jobs_running <= MAX_JOINED_PARSE_JOBS);
                }
                assert_eq!(view.joined_parse_jobs_running, MAX_JOINED_PARSE_JOBS);
                assert_eq!(view.joined_parse_jobs.len(), MAX_JOINED_PARSE_JOBS);
                if switch_document {
                    // Repeat real session replacement, which clears the block
                    // map but must leave detached work charged against the cap.
                    for title in ["Second", "Third"] {
                        view.sessions.open_untitled(&text, title);
                        view.on_document_replaced();
                        let index = BlockIndex::from_buffer(view.editor().document());
                        for ordinal in 0..8 {
                            view.schedule_joined_parse(&[index.block(ordinal).unwrap()], cx);
                        }
                        assert!(view.joined_parse_jobs.is_empty());
                        assert_eq!(view.joined_parse_jobs_running, MAX_JOINED_PARSE_JOBS);
                    }
                }
                let document = view.sessions.active().editor().document();
                let index = BlockIndex::from_buffer(document);
                let current = index.block(7).unwrap();
                view.block_index
                    .publish(index, IndexSource::Formal, document);
                current
            });
            // Model the render wakeup: only the final viewport is requested
            // again when capacity is released, including old-document results.
            let wakeups = std::rc::Rc::new(std::cell::Cell::new(0));
            let observed_wakeups = wakeups.clone();
            let _subscription = cx.update(|cx| {
                cx.observe(&view, move |view, cx| {
                    observed_wakeups.set(observed_wakeups.get() + 1);
                    view.update(cx, |view, cx| {
                        view.schedule_joined_parse(&[current], cx);
                        assert!(view.joined_parse_jobs_running <= MAX_JOINED_PARSE_JOBS);
                    });
                })
            });
            cx.run_until_parked();
            assert!(
                wakeups.get() > 0,
                "completion must wake the current viewport"
            );
            view.update(cx, |view, _| {
                assert_eq!(view.joined_parse_jobs_running, 0);
                assert!(view.joined_parse_jobs.is_empty());
                let cached = &view.joined_parse_cache[&current.id];
                assert_eq!(cached.revision, view.editor().document().revision());
                assert_eq!(cached.source_range, current.source_range);
                // No backlog: traversed intermediate blocks were never parsed.
                let index = view.current_index().unwrap();
                for ordinal in MAX_JOINED_PARSE_JOBS..7 {
                    assert!(
                        !view
                            .joined_parse_cache
                            .contains_key(&index.block(ordinal).unwrap().id)
                    );
                }
                if switch_document {
                    assert_eq!(view.joined_parse_cache.len(), 1);
                }
            });
        }
    }

    #[gpui::test]
    fn joined_parse_completion_rejects_an_old_revision_without_evicting_current_rows(
        cx: &mut gpui::TestAppContext,
    ) {
        let text = format!(
            "before **{}\nmiddle\nend**\n",
            "x".repeat(crate::line::JOIN_SYNC_BYTE_BUDGET)
        );
        let view = gpui::AppContext::new(cx, |cx| EditorView::new(&text, "Untitled", cx));
        let id = view.update(cx, |view, cx| {
            let document = view.sessions.active().editor().document();
            let index = BlockIndex::from_buffer(document);
            let indexed = index.block(0).unwrap();
            view.schedule_joined_parse(&[indexed], cx);
            view.editor_mut().insert_text("new ").unwrap();
            let document = view.sessions.active().editor().document();
            view.block_index.publish(
                BlockIndex::from_buffer(document),
                IndexSource::Formal,
                document,
            );
            let indexed = view.current_index().unwrap().block(0).unwrap();
            let span = block_line_span(view.editor().document(), &indexed).unwrap();
            let parse = parse_joined_span(
                view.editor().document(),
                span,
                indexed.source_range,
                view.editor().document().revision(),
            )
            .unwrap();
            // A newer request may already have completed after switching away
            // and back. The late old request must preserve this exact snapshot.
            view.joined_parse_cache.insert(
                indexed.id,
                JoinedBlockCache {
                    revision: view.editor().document().revision(),
                    source_range: indexed.source_range,
                    parse,
                },
            );
            view.cached_block(&indexed, &(1..2)).unwrap();
            indexed.id
        });
        cx.run_until_parked();
        view.update(cx, |view, _| {
            assert!(!view.joined_parse_jobs.contains_key(&id));
            assert_eq!(
                view.joined_parse_cache[&id].revision,
                view.editor().document().revision()
            );
            assert!(
                view.block_cache.contains_key(&id),
                "a rejected result must not evict current rows"
            );
        });
    }

    #[gpui::test]
    fn joined_parse_completion_requires_the_current_block_identity_and_range(
        cx: &mut gpui::TestAppContext,
    ) {
        for change_identity in [true, false] {
            let text = format!(
                "{}\nsecond\n",
                "x".repeat(crate::line::JOIN_SYNC_BYTE_BUDGET)
            );
            let view = gpui::AppContext::new(cx, |cx| EditorView::new(&text, "Untitled", cx));
            let id = view.update(cx, |view, cx| {
                let document = view.sessions.active().editor().document();
                view.block_index.publish(
                    BlockIndex::from_buffer(document),
                    IndexSource::Formal,
                    document,
                );
                let mut request = view.current_index().unwrap().block(0).unwrap();
                // Model a request whose old block boundaries/identity disagree
                // with the index that is current when the worker finishes.
                if change_identity {
                    request.id = BlockId(999);
                } else {
                    request.source_range.end.0 -= 1;
                }
                view.schedule_joined_parse(&[request], cx);
                assert!(view.joined_parse_jobs.contains_key(&request.id));
                request.id
            });
            cx.run_until_parked();
            view.update(cx, |view, _| {
                assert!(!view.joined_parse_jobs.contains_key(&id));
                assert!(view.joined_parse_cache.is_empty());
            });
        }
    }

    #[gpui::test]
    fn joined_parse_completion_does_not_clear_another_snapshots_pending_job(
        cx: &mut gpui::TestAppContext,
    ) {
        for switch_document in [true, false] {
            let text = format!(
                "{}\nsecond\n",
                "x".repeat(crate::line::JOIN_SYNC_BYTE_BUDGET)
            );
            let view = gpui::AppContext::new(cx, |cx| EditorView::new(&text, "Untitled", cx));
            let (id, current_job) = view.update(cx, |view, cx| {
                let indexed = BlockIndex::from_buffer(view.editor().document())
                    .block(0)
                    .unwrap();
                view.schedule_joined_parse(&[indexed], cx);
                if switch_document {
                    view.sessions.open_untitled(&text, "Other");
                    view.on_document_replaced();
                } else {
                    view.editor_mut().insert_text("new ").unwrap();
                }
                let current_job = JoinedParseJob {
                    document: view.document_key(),
                    revision: view.editor().document().revision(),
                    source_range: indexed.source_range,
                };
                view.joined_parse_jobs.insert(indexed.id, current_job);
                (indexed.id, current_job)
            });
            cx.run_until_parked();
            view.update(cx, |view, _| {
                assert_eq!(view.joined_parse_jobs.get(&id), Some(&current_job));
                assert!(view.joined_parse_cache.is_empty());
            });
        }
    }

    #[gpui::test]
    fn disclosures_are_current_reuses_a_joined_runs_shared_disclosure(
        cx: &mut gpui::TestAppContext,
    ) {
        // A caret sitting inside "bold" on the first physical line discloses
        // the whole shared Strong construct, including its closing marker on
        // the second physical line, which therefore caches a disclosure of
        // its own even though the caret never touches it (see
        // `merged_disclosure`). Comparing that line's cached disclosure
        // against its *own*, unmerged disclosure would never match while the
        // caret sits here, rebuilding the block on every frame instead of
        // reusing it.
        let text = "This is **bold\nacross lines** ok\n";
        let view = gpui::AppContext::new(cx, |cx| EditorView::new(text, "Untitled", cx));
        view.update(cx, |view, _cx| {
            let caret = SourceOffset(text.find("bold").unwrap());
            view.editor_mut()
                .set_selection(Selection::caret(caret))
                .unwrap();
            let indexed = view.block_at_offset(caret).unwrap();
            let span = block_line_span(view.editor().document(), &indexed).unwrap();
            let visual = presented_block(view.editor(), &indexed, &span, None).unwrap();
            assert_eq!(
                visual.lines.len(),
                3,
                "the joined run's two lines plus the trailing empty line the final newline owns"
            );
            assert!(
                visual.lines[1].disclosure.is_some(),
                "the line without its own caret still carries the run's merged disclosure"
            );
            assert!(
                view.disclosures_are_current(&indexed, &visual),
                "an unmoved caret must not invalidate a joined run's cached disclosure"
            );

            // Moving off the shared construct entirely must still invalidate
            // the cache: the fix must not make disclosures look current
            // unconditionally.
            let elsewhere = SourceOffset(text.find(" ok").unwrap());
            view.editor_mut()
                .set_selection(Selection::caret(elsewhere))
                .unwrap();
            assert!(
                !view.disclosures_are_current(&indexed, &visual),
                "moving off the disclosed construct must still invalidate the cache"
            );
        });
    }

    #[gpui::test]
    fn cached_block_reuses_a_joined_runs_presentation_across_frames(cx: &mut gpui::TestAppContext) {
        // `disclosures_are_current` is only a guard; the actual per-frame path
        // is `cached_block`, which the render loop calls every frame. This
        // exercises that call site directly so a wiring regression there —
        // not just a regression in the helper itself — would be caught.
        let text = "This is **bold\nacross lines** ok\n";
        let view = gpui::AppContext::new(cx, |cx| EditorView::new(text, "Untitled", cx));
        view.update(cx, |view, _cx| {
            let caret = SourceOffset(text.find("bold").unwrap());
            view.editor_mut()
                .set_selection(Selection::caret(caret))
                .unwrap();
            let indexed = view.block_at_offset(caret).unwrap();
            let span = block_line_span(view.editor().document(), &indexed).unwrap();

            let (_, first_reused) = view.cached_block(&indexed, &span).unwrap();
            assert!(!first_reused, "nothing was cached yet");

            let (_, second_reused) = view.cached_block(&indexed, &span).unwrap();
            assert!(
                second_reused,
                "an unmoved caret inside a joined run must reuse the cached \
                 presentation instead of rebuilding it every frame"
            );

            let elsewhere = SourceOffset(text.find(" ok").unwrap());
            view.editor_mut()
                .set_selection(Selection::caret(elsewhere))
                .unwrap();
            let (_, third_reused) = view.cached_block(&indexed, &span).unwrap();
            assert!(
                !third_reused,
                "moving off the disclosed construct must still rebuild the presentation"
            );
        });
    }

    #[gpui::test]
    fn list_enter_keeps_semantic_depth_until_the_next_text_edit(cx: &mut gpui::TestAppContext) {
        for (source, indentation, depth, marker_x, typed, expected) in [
            ("- abc", "", 1, 0.0, "- def", "- abc\n- def"),
            ("- abc", "", 1, 0.0, "  - xyz", "- abc\n  - xyz"),
            (
                "- abc\n  - xyz",
                "  ",
                2,
                24.0,
                "- def",
                "- abc\n  - xyz\n  - def",
            ),
            (
                "10. abc\n    1. xyz",
                "    ",
                2,
                24.0,
                "- def",
                "10. abc\n    1. xyz\n    - def",
            ),
            ("10. abc", "", 1, 0.0, "20. def", "10. abc\n20. def"),
        ] {
            let view = gpui::AppContext::new(cx, |cx| EditorView::new(source, "Untitled", cx));
            view.update(cx, |view, cx| {
                let end = SourceOffset(source.len());
                view.editor_mut()
                    .set_selection(Selection::caret(end))
                    .unwrap();
                let context = view
                    .list_editing_context(ListCaretOrigin::Marker)
                    .expect("list item end has semantic editing context");
                assert_eq!(context.indentation, indentation);
                assert_eq!(context.owner.depth, depth);

                view.insert_newline(ListCaretOrigin::Marker, cx);
                assert_eq!(view.editor().document().full_text(), format!("{source}\n"));
                assert_eq!(
                    view.editor().selection().active,
                    SourceOffset(source.len() + 1)
                );

                let pending = view
                    .pending_list_editing
                    .clone()
                    .expect("newline keeps the owner until text input");
                let line = view
                    .editor()
                    .document()
                    .line_for_offset(pending.offset)
                    .unwrap();
                let indexed = view.block_at_offset(pending.offset).unwrap();
                let mut visual = presented_block_with_list_projection(
                    view.editor(),
                    &indexed,
                    &(line.0..line.0 + 1),
                    None,
                    None,
                    view.line_height(),
                )
                .expect("the empty line presents");
                assert!(apply_list_editing_context(&mut visual, &pending));
                let layout = layout_block(&visual, 400.0, &FixedAdvanceShaper::default());
                let point = layout
                    .point_for_source(&visual, pending.offset, &FixedAdvanceShaper::default())
                    .expect("the empty line owns the caret");
                assert_eq!(point.x, marker_x);
                assert_eq!(
                    layout.source_for_point(
                        &visual,
                        point.x,
                        point.y,
                        &FixedAdvanceShaper::default()
                    ),
                    Some(pending.offset)
                );

                view.insert_text(typed, cx);
                assert_eq!(view.editor().document().full_text(), expected);
                assert!(view.pending_list_editing.is_none());
            });
        }

        let source = "- first\n- abc\n- last";
        let view = gpui::AppContext::new(cx, |cx| EditorView::new(source, "Untitled", cx));
        view.update(cx, |view, cx| {
            let caret = SourceOffset("- first\n- abc".len());
            view.editor_mut()
                .set_selection(Selection::caret(caret))
                .unwrap();
            view.insert_newline(ListCaretOrigin::Marker, cx);
            view.insert_text("- def", cx);
            assert_eq!(
                view.editor().document().full_text(),
                "- first\n- abc\n- def\n- last"
            );
        });
    }

    #[gpui::test]
    fn list_enter_variants_keep_continuation_and_history_contracts(cx: &mut gpui::TestAppContext) {
        let view = gpui::AppContext::new(cx, |cx| EditorView::new("- abc", "Untitled", cx));
        view.update(cx, |view, cx| {
            view.editor_mut()
                .set_selection(Selection::caret(SourceOffset(5)))
                .unwrap();
            view.insert_newline(ListCaretOrigin::Body, cx);
            let context = view.pending_list_editing.as_ref().unwrap();
            let indexed = view.block_at_offset(context.offset).unwrap();
            let line = view
                .editor()
                .document()
                .line_for_offset(context.offset)
                .unwrap();
            let mut visual = presented_block_with_list_projection(
                view.editor(),
                &indexed,
                &(line.0..line.0 + 1),
                None,
                None,
                view.line_height(),
            )
            .unwrap();
            assert!(apply_list_editing_context(&mut visual, context));
            let layout = layout_block(&visual, 400.0, &FixedAdvanceShaper::default());
            assert_eq!(
                layout
                    .point_for_source(&visual, context.offset, &FixedAdvanceShaper::default())
                    .unwrap()
                    .x,
                16.0
            );

            view.insert_text("def", cx);
            assert_eq!(view.editor().document().full_text(), "- abc\ndef");
        });

        let view = gpui::AppContext::new(cx, |cx| EditorView::new("- abc", "Untitled", cx));
        view.update(cx, |view, cx| {
            view.editor_mut()
                .set_selection(Selection::caret(SourceOffset(5)))
                .unwrap();
            view.insert_newline(ListCaretOrigin::Marker, cx);
            view.dispatch(EditorCommand::Backspace, cx);
            assert_eq!(view.editor().document().full_text(), "- abc");
            assert_eq!(view.editor().selection(), Selection::caret(SourceOffset(5)));

            view.insert_newline(ListCaretOrigin::Marker, cx);
            view.insert_newline(ListCaretOrigin::Marker, cx);
            assert_eq!(view.editor().document().full_text(), "- abc\n\n");
            assert!(view.pending_list_editing.is_none());
        });

        let view = gpui::AppContext::new(cx, |cx| EditorView::new("- abc", "Untitled", cx));
        view.update(cx, |view, cx| {
            view.editor_mut()
                .set_selection(Selection::caret(SourceOffset(5)))
                .unwrap();
            view.insert_newline(ListCaretOrigin::Marker, cx);
            view.insert_text("- def", cx);
            let after = view.editor().document().full_text();
            assert_eq!(after, "- abc\n- def");
            view.dispatch(EditorCommand::Undo, cx);
            assert_eq!(view.editor().document().full_text(), "- abc\n");
            view.dispatch(EditorCommand::Redo, cx);
            assert_eq!(view.editor().document().full_text(), after);
        });
    }

    #[gpui::test]
    fn list_enter_preserves_caret_geometry_when_the_new_row_owns_an_existing_line_ending(
        cx: &mut gpui::TestAppContext,
    ) {
        for (source, line_ending_len) in [
            ("- first\n- abc\n- last", 1),
            ("- abc\n", 1),
            ("- abc\r\n", 2),
            ("- abc\r", 1),
        ] {
            let view = gpui::AppContext::new(cx, |cx| EditorView::new(source, "Untitled", cx));
            view.update(cx, |view, cx| {
                let caret = source
                    .find("- abc")
                    .map(|offset| offset + "- abc".len())
                    .unwrap_or_else(|| source.trim_end_matches('\n').len());
                view.editor_mut()
                    .set_selection(Selection::caret(SourceOffset(caret)))
                    .unwrap();
                view.insert_newline(ListCaretOrigin::Marker, cx);

                let pending = view
                    .pending_list_editing
                    .clone()
                    .expect("list newline keeps transient context");
                assert_eq!(pending.line_ending_len, line_ending_len);
                let indexed = view.block_at_offset(pending.offset).unwrap();
                let line = view
                    .editor()
                    .document()
                    .line_for_offset(pending.offset)
                    .unwrap();
                let mut visual = presented_block_with_list_projection(
                    view.editor(),
                    &indexed,
                    &(line.0..line.0 + 1),
                    None,
                    None,
                    view.line_height(),
                )
                .expect("existing line ending row presents");
                assert!(apply_list_editing_context(&mut visual, &pending));

                for (origin, expected_x) in [
                    (ListCaretOrigin::Marker, 0.0),
                    (ListCaretOrigin::Body, 16.0),
                ] {
                    let mut context = pending.clone();
                    context.caret_origin = origin;
                    assert!(apply_list_editing_context(&mut visual, &context));
                    let layout = layout_block(&visual, 400.0, &FixedAdvanceShaper::default());
                    let point = layout
                        .point_for_source(&visual, pending.offset, &FixedAdvanceShaper::default())
                        .expect("line ending row owns the caret");
                    assert_eq!(point.x, expected_x);
                    assert_eq!(
                        layout.source_for_point(
                            &visual,
                            point.x,
                            point.y,
                            &FixedAdvanceShaper::default(),
                        ),
                        Some(pending.offset)
                    );
                }
            });
        }
    }

    #[test]
    fn every_line_resolves_to_its_markdown_block() {
        let source = "# title\n\nparagraph one\ncontinued\n\n```rust\nlet x = 1;\n```\n\ntail\n";
        let mut document = RopeBuffer::from_text(source);
        let mut index = BlockIndex::from_buffer(&document);
        let kinds = (0..document.line_count())
            .map(|line| block_at_line(&index, &document, line).map(|block| block.kind))
            .collect::<Vec<_>>();
        assert_eq!(
            kinds,
            vec![
                Some(hane_markdown::NodeKind::Heading(1)),
                Some(hane_markdown::NodeKind::Heading(1)),
                Some(hane_markdown::NodeKind::Paragraph),
                Some(hane_markdown::NodeKind::Paragraph),
                Some(hane_markdown::NodeKind::Paragraph),
                Some(hane_markdown::NodeKind::CodeBlock),
                Some(hane_markdown::NodeKind::CodeBlock),
                Some(hane_markdown::NodeKind::CodeBlock),
                Some(hane_markdown::NodeKind::CodeBlock),
                Some(hane_markdown::NodeKind::Paragraph),
                Some(hane_markdown::NodeKind::Paragraph),
            ],
            "each line reports the block it sits in"
        );

        // After an edit the incremental index still answers for every line, and
        // the fenced block still owns its interior lines.
        let base = document.revision();
        document.edit(SourceRange::empty(9), "extra ").unwrap();
        let deltas = document.deltas_since(base).unwrap();
        index.update(&document, &deltas);
        assert_eq!(
            block_at_line(&index, &document, 6).map(|block| block.kind),
            Some(hane_markdown::NodeKind::CodeBlock)
        );
        assert!(
            (0..document.line_count()).all(|line| block_at_line(&index, &document, line).is_some())
        );
    }

    /// Ten columns at the test shaper's 8 px advance.
    const TEST_WIDTH: f32 = 80.0;

    fn laid_out(
        editor: &Editor,
        index: &BlockIndex,
        ordinal: usize,
        shaper: &FixedAdvanceShaper,
    ) -> (VisualBlock, BlockLayout) {
        let block = presented_block(
            editor,
            &index.block(ordinal).unwrap(),
            &(0..usize::MAX),
            None,
        )
        .expect("block presents");
        let layout = layout_block(&block, TEST_WIDTH, shaper);
        (block, layout)
    }

    #[test]
    fn moving_down_past_a_block_lands_on_the_first_row_of_the_next() {
        let editor = Editor::new("alpha\n\nbravo charlie delta echo\n");
        let index = BlockIndex::from_buffer(editor.document());
        let shaper = FixedAdvanceShaper::new(8.0);
        let (first, layout) = laid_out(&editor, &index, 0, &shaper);
        let x = 3.0 * 8.0;

        // The blank line tiling folded into the first block is still a row of
        // it, so one move down stays inside the block.
        let VerticalMove::To(blank) =
            layout.vertical_target(&first, SourceOffset(3), true, x, &shaper)
        else {
            panic!("the blank line is a row of the first block");
        };
        assert_eq!(
            layout.vertical_target(&first, blank, true, x, &shaper),
            VerticalMove::PastEdge,
            "the blank line is the last row of the block"
        );

        let target = neighbor_row_target(
            &editor,
            Some(&index),
            &first,
            true,
            x,
            TEST_WIDTH,
            &shaper,
            None,
        )
        .expect("there is a block below");
        let (second, below) = laid_out(&editor, &index, 1, &shaper);
        let point = below
            .point_for_source(&second, target, &shaper)
            .expect("the target is in the block below");
        assert_eq!(
            (point.row, point.x),
            (0, x),
            "the caret lands on the first row of the next block, at the x it was aiming at"
        );
    }

    #[test]
    fn moving_up_past_a_block_lands_on_its_last_row() {
        // A heading wide enough to wrap, so the block above has more rows than
        // it has source lines and only the last of them is the target. A heading
        // ends its block at its own line, so no blank row sits between the two.
        let editor = Editor::new("# one two three four five six\nsecond block\n");
        let index = BlockIndex::from_buffer(editor.document());
        let shaper = FixedAdvanceShaper::new(8.0);
        let (second, _) = laid_out(&editor, &index, 1, &shaper);
        let (first, above) = laid_out(&editor, &index, 0, &shaper);
        assert!(
            above.lines.len() > 1,
            "the heading above wraps onto several rows"
        );

        let target = neighbor_row_target(
            &editor,
            Some(&index),
            &second,
            false,
            0.0,
            TEST_WIDTH,
            &shaper,
            None,
        )
        .expect("there is a block above");
        assert_eq!(
            above
                .point_for_source(&first, target, &shaper)
                .map(|point| point.row),
            Some(above.lines.len() - 1),
            "moving up enters the block above on its last row, not its last line"
        );
    }

    #[test]
    fn the_document_edges_have_no_neighbor_row() {
        let editor = Editor::new(
            "only block
",
        );
        let index = BlockIndex::from_buffer(editor.document());
        let shaper = FixedAdvanceShaper::new(8.0);
        let (block, _) = laid_out(&editor, &index, 0, &shaper);
        for down in [true, false] {
            assert_eq!(
                neighbor_row_target(
                    &editor,
                    Some(&index),
                    &block,
                    down,
                    0.0,
                    TEST_WIDTH,
                    &shaper,
                    None,
                ),
                None
            );
        }
    }

    #[test]
    fn moving_into_a_joinable_neighbor_beyond_the_sync_budget_uses_the_cached_parse() {
        // The neighbor below `first` is a paragraph whose two `**` markers sit
        // more than `JOIN_SYNC_LINE_BUDGET` lines apart, so a one-line render
        // window on it cannot resolve them without a cached whole-span parse
        // (see `a_huge_joinable_paragraph_presents_only_its_visible_window_
        // without_a_cached_parse` in `hane_ui::line`). `neighbor_row_target`
        // must thread a caller-supplied `JoinedParse` through to
        // `presented_block` — exactly what `EditorView::neighbor_row_target`
        // does once it resolves the neighbor and consults `joined_parse_cache`
        // — instead of always running the one-line, no-parse path a
        // navigation-only policy of its own would.
        let mut source = String::from("alpha\n\n**bold\n");
        for line in 0..(JOIN_SYNC_LINE_BUDGET + 200) {
            source.push_str(&format!("filler line {line}\n"));
        }
        source.push_str("end**\n");
        let editor = Editor::new(&source);
        let index = BlockIndex::from_buffer(editor.document());
        let shaper = FixedAdvanceShaper::new(8.0);
        let (first, _) = laid_out(&editor, &index, 0, &shaper);

        let second = index.block(1).expect("second block");
        let document = editor.document();
        let span = block_line_span(document, &second).expect("block spans lines");
        let content_end = span.end - trailing_blank_lines(document, &span);
        let joined = parse_joined_span(
            document,
            span.start..content_end,
            second.source_range,
            document.revision(),
        )
        .expect("the whole span parses");

        // Aimed at the 3rd column: inside "bold" once the opening `**` hides
        // as a marker, but still inside the literal `**` when it does not —
        // the same x lands on a different source offset in each case.
        let x = 2.0 * 8.0;
        let target = neighbor_row_target(
            &editor,
            Some(&index),
            &first,
            true,
            x,
            TEST_WIDTH,
            &shaper,
            Some(&joined),
        )
        .expect("there is a block below");

        let without_cache = neighbor_row_target(
            &editor,
            Some(&index),
            &first,
            true,
            x,
            TEST_WIDTH,
            &shaper,
            None,
        )
        .expect("there is still a block below without the cache");
        assert_ne!(
            target, without_cache,
            "without the cached parse the opening `**` stays literal instead \
             of hiding as a marker, so the same aim point must land on a \
             different source offset"
        );

        // The one-line neighbor window must resolve to the same source offset
        // a wider window over the same block would, once both share the
        // cached whole-span parse — the same guarantee
        // `one_physical_line_render_window_with_cached_joined_parse_matches_a_
        // wider_window` establishes for `present_block` itself, carried
        // through navigation's own call path.
        let wide_window = span.start..(span.start + 5).min(content_end.max(span.start + 1));
        let wide_visual = presented_block(&editor, &second, &wide_window, Some(&joined))
            .expect("the wide window presents");
        let wide_layout = layout_block(&wide_visual, TEST_WIDTH, &shaper);
        let wide_target = wide_layout
            .source_at_x(&wide_visual, 0, x, &shaper)
            .expect("row 0 exists in the wide window too");
        assert_eq!(
            target, wide_target,
            "the neighbor's one-line window must resolve the same source offset \
             a wider window would, once both share the cached whole-span parse"
        );
    }

    #[test]
    fn block_context_rejects_stale_revision() {
        assert!(block_context_revision_is_current(Revision(4), Revision(4)));
        assert!(!block_context_revision_is_current(Revision(5), Revision(4)));
    }

    #[test]
    fn height_anchor_rebases_without_scanning_untouched_blocks() {
        let inserted = [
            HeightBlock {
                id: BlockId(20),
                line_count: 1,
            },
            HeightBlock {
                id: BlockId(21),
                line_count: 1,
            },
            HeightBlock {
                id: BlockId(22),
                line_count: 1,
            },
        ];
        assert_eq!(
            rebase_ordinal_after_splice(2, Some(BlockId(2)), 4..6, &inserted, 9),
            2
        );
        assert_eq!(
            rebase_ordinal_after_splice(7, Some(BlockId(7)), 4..6, &inserted, 9),
            8
        );
        assert_eq!(
            rebase_ordinal_after_splice(5, Some(BlockId(21)), 4..6, &inserted, 9),
            5
        );
        assert_eq!(
            rebase_ordinal_after_splice(5, Some(BlockId(99)), 4..6, &inserted, 9),
            4
        );
    }

    #[test]
    fn height_block_metadata_splices_across_chunk_boundaries() {
        let mut flat = (0..300)
            .map(|id| HeightBlock {
                id: BlockId(id),
                line_count: id as usize % 5 + 1,
            })
            .collect::<Vec<_>>();
        let mut blocks = flat.iter().copied().collect::<HeightBlocks>();
        let inserted = [
            HeightBlock {
                id: BlockId(1_000),
                line_count: 2,
            },
            HeightBlock {
                id: BlockId(1_001),
                line_count: 3,
            },
            HeightBlock {
                id: BlockId(1_002),
                line_count: 4,
            },
        ];
        flat.splice(127..131, inserted);
        blocks.splice(127..131, &inserted);
        assert_eq!(blocks.len(), flat.len());
        assert!(blocks.range_eq(0..flat.len(), &flat));
        assert_eq!(blocks.get(126), flat.get(126).copied());
        assert_eq!(blocks.get(130), flat.get(130).copied());
    }

    fn draft_test_root(label: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQUENCE: AtomicU64 = AtomicU64::new(0);
        std::env::temp_dir().join(format!(
            "hane-draft-save-{label}-{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ))
    }

    // Regression test for the P1 review finding on `schedule_draft_save`:
    // switching to another note within the 750ms debounce window used to
    // cancel the pending write outright, because the timer only saved when
    // its own session was still the active one. Typing into an unnamed note
    // and switching away before the timer fires must still journal what was
    // typed.
    #[gpui::test]
    fn draft_save_survives_switching_sessions_within_the_debounce_window(
        cx: &mut gpui::TestAppContext,
    ) {
        let root = draft_test_root("switch");
        std::fs::create_dir_all(&root).unwrap();
        let work_folder = OsWorkFolderScanner.scan(&root).unwrap();

        let view = gpui::AppContext::new(cx, |cx| {
            EditorView::from_sessions(
                SessionSet::with_untitled("", "Untitled"),
                Arc::new(OsFileService),
                StateStores::memory(),
                cx,
            )
        });

        view.update(cx, |view, cx| {
            view.work_folder = Some(work_folder);
            // Start the first unnamed note and type into it.
            view.new_work_folder_note(cx);
            view.editor_mut()
                .insert_text("today I thought about this design")
                .unwrap();
            view.after_input(cx);
            // Switch away to a second unnamed note before the debounce timer
            // for the first one fires.
            view.new_work_folder_note(cx);
        });

        // `schedule_draft_save` debounces on a real `gpui::Timer` (wall-clock,
        // not the deterministic test dispatcher), so the test has to wait for
        // real time to pass rather than fast-forwarding a virtual clock. The
        // spawned task must run once first to reach its `Timer::after` await
        // and register with the real clock before that wait is worth doing.
        cx.run_until_parked();
        std::thread::sleep(Duration::from_millis(900));
        cx.run_until_parked();

        let recovered = OsDraftStore.recover(&root).unwrap();
        assert_eq!(recovered.drafts.len(), 1);
        assert_eq!(
            recovered.drafts[0].text,
            "today I thought about this design"
        );

        std::fs::remove_dir_all(&root).unwrap();
    }

    // Issue #2 follow-up: `prompt_open_work_folder` is the GUI entry point
    // for switching into work-folder mode at runtime (previously only
    // reachable via a startup CLI argument). `switch_to_work_folder` is the
    // part of it that does not depend on the native folder picker: it must
    // drop whatever was open before and start fresh on the new root, rather
    // than leaving stale sessions or per-session bookkeeping (like an old
    // folder's unnamed-draft mapping) behind.
    #[gpui::test]
    fn switch_to_work_folder_discards_previous_sessions_and_opens_the_new_root(
        cx: &mut gpui::TestAppContext,
    ) {
        let old_root = draft_test_root("switch-old");
        std::fs::create_dir_all(&old_root).unwrap();
        let old_work_folder = OsWorkFolderScanner.scan(&old_root).unwrap();

        let new_root = draft_test_root("switch-new");
        std::fs::create_dir_all(&new_root).unwrap();
        std::fs::write(new_root.join("New.md"), "# New\n").unwrap();

        let view = gpui::AppContext::new(cx, |cx| {
            EditorView::from_sessions(
                SessionSet::with_untitled("", "Untitled"),
                Arc::new(OsFileService),
                StateStores::memory(),
                cx,
            )
        });

        view.update(cx, |view, cx| {
            view.work_folder = Some(old_work_folder);
            view.new_work_folder_note(cx);
            view.editor_mut()
                .insert_text("draft in the old folder")
                .unwrap();
            view.after_input(cx);
        });
        assert_eq!(
            view.read_with(cx, |view, _| view.work_folder_drafts.len()),
            1
        );

        view.update(cx, |view, cx| {
            view.switch_to_work_folder(new_root.clone(), cx);
        });
        cx.run_until_parked();

        view.read_with(cx, |view, _| {
            assert!(
                view.work_folder_drafts.is_empty(),
                "the old folder's draft bookkeeping must not leak into the new one"
            );
            assert_eq!(
                view.work_folder.as_ref().map(WorkFolder::root),
                Some(new_root.as_path())
            );
            assert_eq!(view.sessions().count(), 1);
            assert_eq!(view.editor().document().full_text().trim(), "# New");
        });

        // The old folder's unnamed draft must have been flushed before its
        // `work_folder_drafts` entry was dropped, or switching away loses it
        // exactly the way a quit inside the debounce window used to.
        let old_recovered = OsDraftStore.recover(&old_root).unwrap();
        assert_eq!(old_recovered.drafts.len(), 1);
        assert_eq!(old_recovered.drafts[0].text, "draft in the old folder");

        std::fs::remove_dir_all(&old_root).unwrap();
        std::fs::remove_dir_all(&new_root).unwrap();
    }

    // Issue #28: Save As on an unnamed note used to fall back to `"."`
    // (resolved against the process's current directory, e.g. the app's own
    // install directory on Windows) whenever the active session had no file
    // yet. With a Work Folder open, the dialog should start there instead.
    #[gpui::test]
    fn save_as_on_an_unnamed_note_starts_in_the_active_work_folder(cx: &mut gpui::TestAppContext) {
        let root = draft_test_root("save-as-unnamed");
        std::fs::create_dir_all(&root).unwrap();
        let work_folder = OsWorkFolderScanner.scan(&root).unwrap();

        let view = gpui::AppContext::new(cx, |cx| {
            EditorView::from_sessions(
                SessionSet::with_untitled("", "Untitled"),
                Arc::new(OsFileService),
                StateStores::memory(),
                cx,
            )
        });

        view.update(cx, |view, cx| {
            view.work_folder = Some(work_folder);
            view.new_work_folder_note(cx);
            view.prompt_save_as(cx);
        });
        cx.run_until_parked();

        cx.simulate_new_path_selection(|directory| {
            assert_eq!(directory, root.as_path());
            None
        });

        std::fs::remove_dir_all(&root).unwrap();
    }

    // Issue #2 follow-up: a background read started via `open_work_folder_entry`
    // (`OpenPolicy::NewSession`, `into: None`) that is still in flight when the
    // user switches to a different work folder used to have no staleness guard
    // at all: `finish_open`'s check only discarded a stale `ReuseActive` result
    // that targeted the *current* active session, which a `None` target can
    // never match. The result was a session for a file from the old folder
    // getting merged into the new folder's `SessionSet` once the read landed.
    // `work_folder_generation` closes that gap.
    #[gpui::test]
    fn a_stale_new_session_read_from_a_replaced_work_folder_is_discarded(
        cx: &mut gpui::TestAppContext,
    ) {
        let old_root = draft_test_root("stale-read-old");
        std::fs::create_dir_all(&old_root).unwrap();
        let stale_path = old_root.join("Stale.md");
        std::fs::write(&stale_path, "# Stale\n").unwrap();
        let old_work_folder = OsWorkFolderScanner.scan(&old_root).unwrap();

        let new_root = draft_test_root("stale-read-new");
        std::fs::create_dir_all(&new_root).unwrap();

        let view = gpui::AppContext::new(cx, |cx| {
            EditorView::from_sessions(
                SessionSet::with_untitled("", "Untitled"),
                Arc::new(OsFileService),
                StateStores::memory(),
                cx,
            )
        });

        // Simulate the sidebar-entry read that was in flight before the
        // switch: capture the generation it was requested under, exactly as
        // `open_with_policy` does, without waiting for it to complete.
        let generation_at_request = view.update(cx, |view, _cx| {
            view.work_folder = Some(old_work_folder);
            view.work_folder_generation
        });
        let loaded = OsFileService.load(&stale_path).unwrap();

        view.update(cx, |view, cx| {
            view.switch_to_work_folder(new_root.clone(), cx);
        });
        cx.run_until_parked();

        view.update(cx, |view, cx| {
            view.finish_open(None, generation_at_request, &stale_path, Ok(loaded), cx);
        });

        view.read_with(cx, |view, _| {
            assert_eq!(
                view.sessions().count(),
                1,
                "a read from the replaced folder must not add a session to the new one"
            );
            assert!(
                view.sessions()
                    .all(|session| session.file().path() != Some(stale_path.as_path())),
                "the stale file must not appear in any session"
            );
        });

        std::fs::remove_dir_all(&old_root).unwrap();
        std::fs::remove_dir_all(&new_root).unwrap();
    }

    // PR review finding: `prompt_open_work_folder`'s guard originally checked
    // only `self.sessions.active().is_dirty()`, but the work-folder sidebar
    // deliberately keeps more than one session open at once
    // (`open_work_folder_entry` gives every visited note its own session), so
    // a dirty *background* session was not guarded against at all — switching
    // folders would silently discard it along with every other open session.
    #[gpui::test]
    fn a_dirty_background_session_blocks_opening_a_different_folder(cx: &mut gpui::TestAppContext) {
        let root = draft_test_root("dirty-background-guard");
        std::fs::create_dir_all(&root).unwrap();
        let work_folder = OsWorkFolderScanner.scan(&root).unwrap();

        let view = gpui::AppContext::new(cx, |cx| {
            EditorView::from_sessions(
                SessionSet::with_untitled("", "Untitled"),
                Arc::new(OsFileService),
                StateStores::memory(),
                cx,
            )
        });

        view.update(cx, |view, cx| {
            view.work_folder = Some(work_folder);
            // Dirty the first (soon to be backgrounded) session, then open a
            // second note so the dirty one is no longer active.
            view.editor_mut()
                .insert_text("unsaved in the background")
                .unwrap();
            view.after_input(cx);
            view.new_work_folder_note(cx);
            assert!(!view.sessions.active().is_dirty());
            assert_eq!(view.sessions().count(), 2);
        });

        view.update(cx, |view, cx| {
            view.prompt_open_work_folder(cx);
        });

        view.read_with(cx, |view, _| {
            assert_eq!(
                view.status.as_deref(),
                Some("Save current changes before opening a folder"),
                "a dirty background session must block the switch even though the active one is clean"
            );
            assert_eq!(
                view.sessions().count(),
                2,
                "the guard must fire before anything about the open sessions changes"
            );
        });

        std::fs::remove_dir_all(&root).unwrap();
    }

    // Issue #2 follow-up: `schedule_draft_save` only writes 750ms after the
    // last keystroke, so an app quit within that window used to lose
    // whatever was typed since the previous write. `flush_pending_drafts` is
    // what the app-quit hook calls to write the draft immediately instead of
    // waiting on the debounce timer.
    #[gpui::test]
    fn flush_pending_drafts_writes_immediately_without_waiting_for_the_debounce(
        cx: &mut gpui::TestAppContext,
    ) {
        let root = draft_test_root("quit-flush");
        std::fs::create_dir_all(&root).unwrap();
        let work_folder = OsWorkFolderScanner.scan(&root).unwrap();

        let view = gpui::AppContext::new(cx, |cx| {
            EditorView::from_sessions(
                SessionSet::with_untitled("", "Untitled"),
                Arc::new(OsFileService),
                StateStores::memory(),
                cx,
            )
        });

        view.update(cx, |view, cx| {
            view.work_folder = Some(work_folder);
            view.new_work_folder_note(cx);
            view.editor_mut()
                .insert_text("quitting before the debounce fires")
                .unwrap();
            view.after_input(cx);
            // No `Timer::after(750ms)` wait: the flush must not depend on it.
            view.flush_pending_drafts();
        });

        let recovered = OsDraftStore.recover(&root).unwrap();
        assert_eq!(recovered.drafts.len(), 1);
        assert_eq!(
            recovered.drafts[0].text,
            "quitting before the debounce fires"
        );

        std::fs::remove_dir_all(&root).unwrap();
    }

    // Regression test for the P2 review finding on `finish_work_folder_scan`:
    // a `DraftStore::recover` failure used to be swallowed via
    // `drafts.unwrap_or_default()`, leaving `status` at `None` exactly as if
    // there had simply been no drafts to recover. The error must be visible.
    #[gpui::test]
    fn a_draft_recovery_failure_is_surfaced_instead_of_silently_dropped(
        cx: &mut gpui::TestAppContext,
    ) {
        let root = draft_test_root("recovery-error");
        std::fs::create_dir_all(&root).unwrap();
        let work_folder = OsWorkFolderScanner.scan(&root).unwrap();

        let view = gpui::AppContext::new(cx, |cx| {
            EditorView::from_sessions(
                SessionSet::with_untitled("", "Untitled"),
                Arc::new(OsFileService),
                StateStores::memory(),
                cx,
            )
        });

        view.update(cx, |view, cx| {
            let error = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied");
            view.finish_work_folder_scan((Ok(work_folder), Err(error)), cx);
        });

        let warning = view.read_with(cx, |view, _| view.draft_recovery_warning.clone());
        assert!(
            warning.is_some_and(|warning| warning.contains("recover")),
            "expected the drafts-recovery error to be surfaced as a warning"
        );

        std::fs::remove_dir_all(&root).unwrap();
    }

    // Regression test for the follow-up P2 finding: a per-file recovery
    // failure (as opposed to the whole directory being unreadable) must also
    // be visible, not just silently drop the one draft that could not be
    // read while saying nothing about it. The drafts that *were* readable
    // must still come back.
    #[gpui::test]
    fn a_partial_draft_recovery_failure_is_surfaced_while_readable_drafts_still_recover(
        cx: &mut gpui::TestAppContext,
    ) {
        let root = draft_test_root("partial-recovery-error");
        std::fs::create_dir_all(&root).unwrap();
        let work_folder = OsWorkFolderScanner.scan(&root).unwrap();

        let view = gpui::AppContext::new(cx, |cx| {
            EditorView::from_sessions(
                SessionSet::with_untitled("", "Untitled"),
                Arc::new(OsFileService),
                StateStores::memory(),
                cx,
            )
        });

        let readable = RecoveredDraft {
            id: DraftId::generate(),
            text: "kept".to_owned(),
        };
        let partial = RecoveredDrafts {
            drafts: vec![readable.clone()],
            failed: 1,
        };

        view.update(cx, |view, cx| {
            view.finish_work_folder_scan((Ok(work_folder), Ok(partial)), cx);
        });

        view.read_with(cx, |view, _| {
            assert!(
                view.draft_recovery_warning
                    .as_deref()
                    .is_some_and(|warning| warning.contains('1')),
                "expected the one unreadable draft to be surfaced as a warning, got {:?}",
                view.draft_recovery_warning
            );
            assert!(
                view.work_folder_drafts
                    .values()
                    .any(|draft| draft.draft_id == readable.id),
                "the readable draft must still be recovered as a session"
            );
        });

        std::fs::remove_dir_all(&root).unwrap();
    }

    // Regression test for the P2 review finding that the recovery warning
    // was overwritten within the same update cycle: when the work folder has
    // a named note, `finish_work_folder_scan` immediately opens it via
    // `open_path`, which drives `status` through "Opening…" and then
    // "Opened" once the background read completes. The warning has to
    // survive that, not just the synchronous part of the scan.
    #[gpui::test]
    fn a_draft_recovery_warning_survives_opening_the_first_named_note(
        cx: &mut gpui::TestAppContext,
    ) {
        let root = draft_test_root("warning-survives-open");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("Meeting.md"), "# Meeting\n").unwrap();
        let work_folder = OsWorkFolderScanner.scan(&root).unwrap();
        assert_eq!(work_folder.entries().len(), 1);

        let view = gpui::AppContext::new(cx, |cx| {
            EditorView::from_sessions(
                SessionSet::with_untitled("", "Untitled"),
                Arc::new(OsFileService),
                StateStores::memory(),
                cx,
            )
        });

        view.update(cx, |view, cx| {
            let error = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied");
            view.finish_work_folder_scan((Ok(work_folder), Err(error)), cx);
        });

        // Let the background read `open_path` kicked off run to completion,
        // driving `status` to "Opened" the same way a real work-folder open
        // does.
        cx.run_until_parked();

        view.read_with(cx, |view, _| {
            assert_eq!(view.status.as_deref(), Some("Opened"));
            assert!(
                view.draft_recovery_warning
                    .as_deref()
                    .is_some_and(|warning| warning.contains("recover")),
                "expected the recovery warning to survive the first note's open completing, got {:?}",
                view.draft_recovery_warning
            );
        });

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn header_status_line_combines_warning_and_status_without_dropping_either() {
        assert_eq!(header_status_line(None, None), None);
        assert_eq!(
            header_status_line(Some("2 drafts could not be recovered"), None),
            Some("2 drafts could not be recovered".to_owned())
        );
        assert_eq!(
            header_status_line(None, Some("Opened")),
            Some("Opened".to_owned())
        );
        // Regression for the P1 review finding: an unconditional priority for
        // the warning would permanently hide a later, safety-relevant status
        // (a save failure, a conflict) once a work folder has raised a
        // recovery warning, since this view only re-scans a work folder once
        // at startup. Both must show.
        assert_eq!(
            header_status_line(
                Some("2 drafts could not be recovered"),
                Some("Save failed: disk full")
            ),
            Some("2 drafts could not be recovered · Save failed: disk full".to_owned())
        );
    }

    // Regression test for the P1 review finding on the header status line: a
    // save failure that happens after a draft-recovery warning was raised
    // must still reach the user, not be hidden behind the warning for the
    // rest of the session.
    #[gpui::test]
    fn a_save_failure_after_a_draft_recovery_warning_is_still_visible(
        cx: &mut gpui::TestAppContext,
    ) {
        let root = draft_test_root("warning-then-save-failure");
        std::fs::create_dir_all(&root).unwrap();
        let work_folder = OsWorkFolderScanner.scan(&root).unwrap();

        let view = gpui::AppContext::new(cx, |cx| {
            EditorView::from_sessions(
                SessionSet::with_untitled("", "Untitled"),
                Arc::new(OsFileService),
                StateStores::memory(),
                cx,
            )
        });

        view.update(cx, |view, cx| {
            let error = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied");
            view.finish_work_folder_scan((Ok(work_folder), Err(error)), cx);
            // A save failure arriving well after the recovery warning was
            // raised (a later autosave, a manual save, a conflict) must not
            // be swallowed by the warning that is still sitting in
            // `draft_recovery_warning`.
            view.status = Some("Save failed: disk full".to_owned());
        });

        view.read_with(cx, |view, _| {
            let combined = header_status_line(
                view.draft_recovery_warning.as_deref(),
                view.status.as_deref(),
            );
            assert!(
                combined
                    .as_deref()
                    .is_some_and(|line| line.contains("Save failed")),
                "expected the save failure to still be visible, got {combined:?}"
            );
            assert!(
                combined
                    .as_deref()
                    .is_some_and(|line| line.contains("recover")),
                "expected the recovery warning to still be visible too, got {combined:?}"
            );
        });

        std::fs::remove_dir_all(&root).unwrap();
    }

    /// Waits for the 750ms wall-clock debounce timers (`schedule_title_sync`
    /// and friends) to fire, the same way `draft_save_survives_switching_…`
    /// above does: they run on a real `gpui::Timer`, not the deterministic
    /// test dispatcher, so real time has to pass.
    fn settle_debounce(cx: &mut gpui::TestAppContext) {
        cx.run_until_parked();
        std::thread::sleep(Duration::from_millis(900));
        cx.run_until_parked();
    }

    // Issue #6: an unnamed work-folder note earns its filename from the
    // first H1 it is given, with no filename prompt.
    #[gpui::test]
    fn a_new_notes_first_h1_names_its_file(cx: &mut gpui::TestAppContext) {
        let root = draft_test_root("h1-create");
        std::fs::create_dir_all(&root).unwrap();
        let work_folder = OsWorkFolderScanner.scan(&root).unwrap();

        let view = gpui::AppContext::new(cx, |cx| {
            EditorView::from_sessions(
                SessionSet::with_untitled("", "Untitled"),
                Arc::new(OsFileService),
                StateStores::memory(),
                cx,
            )
        });

        view.update(cx, |view, cx| {
            view.work_folder = Some(work_folder);
            view.new_work_folder_note(cx);
            view.editor_mut().insert_text("# LangChain4j").unwrap();
            view.after_input(cx);
        });

        settle_debounce(cx);

        view.read_with(cx, |view, _| {
            assert_eq!(
                view.active_session().path(),
                Some(root.join("LangChain4j.md").as_path())
            );
            assert_eq!(view.active_session().auto_title(), Some("LangChain4j"));
            assert!(!view.active_session().is_dirty());
            // The sidebar renders `work_folder.entries()`; a note created
            // from its H1 must appear there right away, without waiting for
            // a full rescan, or it is unreachable once the user switches
            // away from it.
            assert!(
                view.work_folder
                    .as_ref()
                    .unwrap()
                    .entry_for_path(&root.join("LangChain4j.md"))
                    .is_some(),
                "the new note must appear in the work folder index"
            );
        });
        assert_eq!(
            std::fs::read_to_string(root.join("LangChain4j.md")).unwrap(),
            "# LangChain4j"
        );
        assert!(
            OsDraftStore.recover(&root).unwrap().drafts.is_empty(),
            "the recovery draft must be retired once the note has a real file"
        );

        std::fs::remove_dir_all(&root).unwrap();
    }

    // Issue #39 review follow-up: once a subfolder has been selected, the
    // root row must still be reachable and re-selectable — otherwise a note
    // or folder started after visiting any subfolder is stuck targeting that
    // subfolder until the work folder is reopened.
    #[gpui::test]
    fn selecting_the_work_folder_root_after_a_subfolder_restores_it_as_the_target(
        cx: &mut gpui::TestAppContext,
    ) {
        let root = draft_test_root("select-root-after-subfolder");
        std::fs::create_dir_all(root.join("dev")).unwrap();
        let work_folder = OsWorkFolderScanner.scan(&root).unwrap();

        let view = gpui::AppContext::new(cx, |cx| {
            EditorView::from_sessions(
                SessionSet::with_untitled("", "Untitled"),
                Arc::new(OsFileService),
                StateStores::memory(),
                cx,
            )
        });

        view.update(cx, |view, cx| {
            view.work_folder = Some(work_folder);
            view.toggle_and_select_work_folder_folder(root.join("dev"), cx);
        });
        view.read_with(cx, |view, _| {
            assert_eq!(
                view.selected_folder.as_deref(),
                Some(root.join("dev").as_path())
            );
        });

        view.update(cx, |view, cx| {
            view.select_work_folder_root(cx);
        });

        view.read_with(cx, |view, _| {
            assert_eq!(
                view.selected_folder, None,
                "selecting the root row must clear the subfolder selection"
            );
            assert_eq!(
                view.target_directory_for_new_entry(),
                view.work_folder
                    .as_ref()
                    .map(|folder| folder.root().to_path_buf()),
                "a new note/folder must now target the work folder root again"
            );
        });

        std::fs::remove_dir_all(&root).unwrap();
    }

    // Issue #47: the sidebar must never show two rows selected at once.
    // Opening a file after selecting a folder must move the highlight onto
    // the file (even though `selected_folder` — the new-entry target — stays
    // put), and explicitly selecting a folder while a file is open must move
    // the highlight back onto the folder despite that file remaining the
    // active session underneath.
    #[gpui::test]
    fn opening_a_file_and_selecting_a_folder_never_both_show_selected(
        cx: &mut gpui::TestAppContext,
    ) {
        let root = draft_test_root("sidebar-exclusive-selection");
        std::fs::create_dir_all(root.join("dev")).unwrap();
        let note = root.join("dev").join("note.md");
        std::fs::write(&note, "# Note").unwrap();
        let work_folder = OsWorkFolderScanner.scan(&root).unwrap();

        let view = gpui::AppContext::new(cx, |cx| {
            EditorView::from_sessions(
                SessionSet::with_untitled("", "Untitled"),
                Arc::new(OsFileService),
                StateStores::memory(),
                cx,
            )
        });

        view.update(cx, |view, cx| {
            view.work_folder = Some(work_folder);
            view.toggle_and_select_work_folder_folder(root.join("dev"), cx);
        });
        view.read_with(cx, |view, _| {
            assert_eq!(view.sidebar_focus, SidebarFocus::Folder);
        });

        // Opening a file must move the highlight onto it, even though the
        // folder just selected remains the target directory for new entries.
        view.update(cx, |view, cx| {
            view.open_work_folder_entry(&note, cx);
        });
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert_eq!(view.sidebar_focus, SidebarFocus::ActiveSession);
            assert_eq!(
                view.selected_folder.as_deref(),
                Some(root.join("dev").as_path()),
                "the folder selected earlier must still be the new-entry target"
            );
        });

        // Explicitly reselecting the folder must move the highlight back,
        // even though the file above is still the active session.
        view.update(cx, |view, cx| {
            view.toggle_and_select_work_folder_folder(root.join("dev"), cx);
        });
        view.read_with(cx, |view, _| {
            assert_eq!(view.sidebar_focus, SidebarFocus::Folder);
            assert_eq!(
                view.active_session().path(),
                Some(note.as_path()),
                "selecting the folder must not close the still-open file"
            );
        });

        view.update(cx, |view, cx| view.open_work_folder_entry(&note, cx));
        view.read_with(cx, |view, _| {
            assert_eq!(view.sidebar_focus, SidebarFocus::ActiveSession);
            assert!(
                !view.active_session_has_sidebar_row(),
                "collapsed folder hides the active file; root is the fallback"
            );
        });
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[gpui::test]
    fn empty_work_folder_has_no_active_sidebar_row(cx: &mut gpui::TestAppContext) {
        let root = draft_test_root("sidebar-empty-root");
        std::fs::create_dir_all(&root).unwrap();
        let view = gpui::AppContext::new(cx, |cx| {
            EditorView::from_sessions(
                SessionSet::with_untitled("", "Untitled"),
                Arc::new(OsFileService),
                StateStores::memory(),
                cx,
            )
        });
        view.update(cx, |view, _| {
            view.work_folder = Some(OsWorkFolderScanner.scan(&root).unwrap());
            assert!(!view.active_session_has_sidebar_row());
        });
        std::fs::remove_dir_all(&root).unwrap();
    }

    // Issue #39: "new folder" creates an empty subfolder through the
    // filesystem boundary (`FileService::create_dir`), adds it to the work
    // folder tree without a rescan, and shows it expanded.
    #[gpui::test]
    fn new_work_folder_folder_creates_a_folder_and_adds_it_to_the_tree_expanded(
        cx: &mut gpui::TestAppContext,
    ) {
        let root = draft_test_root("new-folder");
        std::fs::create_dir_all(&root).unwrap();
        let work_folder = OsWorkFolderScanner.scan(&root).unwrap();

        let view = gpui::AppContext::new(cx, |cx| {
            EditorView::from_sessions(
                SessionSet::with_untitled("", "Untitled"),
                Arc::new(OsFileService),
                StateStores::memory(),
                cx,
            )
        });

        view.update(cx, |view, cx| {
            view.work_folder = Some(work_folder);
            view.new_work_folder_folder(cx);
        });
        cx.run_until_parked();

        let expected = root.join("New Folder");
        assert!(expected.is_dir(), "the folder must exist on disk");
        view.read_with(cx, |view, _| {
            let work_folder = view.work_folder.as_ref().unwrap();
            let created = work_folder.children().iter().find_map(|node| match node {
                WorkFolderNode::Folder(folder) if folder.path() == expected => Some(folder),
                _ => None,
            });
            assert!(
                created.is_some(),
                "the new folder must appear in the tree without a rescan"
            );
            assert!(
                view.expanded_folders.contains(&expected),
                "a freshly created folder must show expanded"
            );
        });

        std::fs::remove_dir_all(&root).unwrap();
    }

    // Issue #39: creating a second folder with the same default name avoids
    // clobbering the first one on disk, the same way `unique_markdown_filename`
    // avoids collisions for notes.
    #[gpui::test]
    fn a_second_new_folder_with_the_same_name_does_not_collide(cx: &mut gpui::TestAppContext) {
        let root = draft_test_root("new-folder-collision");
        std::fs::create_dir_all(&root).unwrap();
        let work_folder = OsWorkFolderScanner.scan(&root).unwrap();

        let view = gpui::AppContext::new(cx, |cx| {
            EditorView::from_sessions(
                SessionSet::with_untitled("", "Untitled"),
                Arc::new(OsFileService),
                StateStores::memory(),
                cx,
            )
        });

        view.update(cx, |view, cx| {
            view.work_folder = Some(work_folder);
            view.new_work_folder_folder(cx);
        });
        cx.run_until_parked();
        view.update(cx, |view, cx| {
            view.new_work_folder_folder(cx);
        });
        cx.run_until_parked();

        assert!(root.join("New Folder").is_dir());
        assert!(root.join("New Folder 2").is_dir());

        std::fs::remove_dir_all(&root).unwrap();
    }

    // Issue #39 review follow-up: two "new folder" clicks landing before the
    // first click's background `create_dir` has completed must still pick
    // two different names. The tree is not updated until each write
    // finishes, so without a synchronous reservation both clicks would see
    // the same empty sibling list and race for "New Folder" — and since
    // `create_dir_all` succeeds even when the directory already exists,
    // that race would silently lose one of the two folders instead of
    // erroring.
    #[gpui::test]
    fn two_new_folder_clicks_before_the_first_completes_still_pick_different_names(
        cx: &mut gpui::TestAppContext,
    ) {
        let root = draft_test_root("new-folder-race");
        std::fs::create_dir_all(&root).unwrap();
        let work_folder = OsWorkFolderScanner.scan(&root).unwrap();

        let view = gpui::AppContext::new(cx, |cx| {
            EditorView::from_sessions(
                SessionSet::with_untitled("", "Untitled"),
                Arc::new(OsFileService),
                StateStores::memory(),
                cx,
            )
        });

        view.update(cx, |view, cx| {
            view.work_folder = Some(work_folder);
            // Both calls run before either background `create_dir` has a
            // chance to land, the same as two rapid clicks.
            view.new_work_folder_folder(cx);
            view.new_work_folder_folder(cx);
        });
        cx.run_until_parked();

        assert!(root.join("New Folder").is_dir());
        assert!(
            root.join("New Folder 2").is_dir(),
            "the second concurrent click must not reuse the first click's name"
        );

        std::fs::remove_dir_all(&root).unwrap();
    }

    // Issue #39: a note started while a subfolder is selected earns its
    // filename inside that subfolder, not the work folder root, once its
    // first H1 lands.
    #[gpui::test]
    fn a_new_notes_first_h1_names_its_file_inside_the_selected_folder(
        cx: &mut gpui::TestAppContext,
    ) {
        let root = draft_test_root("h1-create-in-folder");
        std::fs::create_dir_all(root.join("dev")).unwrap();
        let work_folder = OsWorkFolderScanner.scan(&root).unwrap();

        let view = gpui::AppContext::new(cx, |cx| {
            EditorView::from_sessions(
                SessionSet::with_untitled("", "Untitled"),
                Arc::new(OsFileService),
                StateStores::memory(),
                cx,
            )
        });

        view.update(cx, |view, cx| {
            view.work_folder = Some(work_folder);
            view.selected_folder = Some(root.join("dev"));
            view.new_work_folder_note(cx);
            view.editor_mut().insert_text("# GPUI").unwrap();
            view.after_input(cx);
        });

        settle_debounce(cx);

        view.read_with(cx, |view, _| {
            assert_eq!(
                view.active_session().path(),
                Some(root.join("dev/GPUI.md").as_path())
            );
        });
        assert_eq!(
            std::fs::read_to_string(root.join("dev/GPUI.md")).unwrap(),
            "# GPUI"
        );

        std::fs::remove_dir_all(&root).unwrap();
    }

    // Issue #39: an H1-driven rename must keep the note in the folder it
    // already lives in, not pull it back to the work folder root — a
    // regression that would otherwise appear once notes could live in
    // subfolders at all.
    #[gpui::test]
    fn editing_an_auto_managed_h1_keeps_the_note_in_its_own_folder(cx: &mut gpui::TestAppContext) {
        let root = draft_test_root("h1-rename-in-folder");
        std::fs::create_dir_all(root.join("dev")).unwrap();
        let work_folder = OsWorkFolderScanner.scan(&root).unwrap();

        let view = gpui::AppContext::new(cx, |cx| {
            EditorView::from_sessions(
                SessionSet::with_untitled("", "Untitled"),
                Arc::new(OsFileService),
                StateStores::memory(),
                cx,
            )
        });

        view.update(cx, |view, cx| {
            view.work_folder = Some(work_folder);
            view.selected_folder = Some(root.join("dev"));
            view.new_work_folder_note(cx);
            view.editor_mut().insert_text("# GPUI").unwrap();
            view.after_input(cx);
        });
        settle_debounce(cx);

        view.update(cx, |view, cx| {
            // Append " Notes" to the H1.
            let end = SourceOffset(view.editor().document().len_bytes().0);
            view.editor_mut()
                .set_selection(Selection::caret(end))
                .unwrap();
            view.editor_mut().insert_text(" Notes").unwrap();
            view.after_input(cx);
        });
        settle_debounce(cx);

        view.read_with(cx, |view, _| {
            assert_eq!(
                view.active_session().path(),
                Some(root.join("dev/GPUI Notes.md").as_path()),
                "the rename must stay inside dev/, not move to the work folder root"
            );
        });

        std::fs::remove_dir_all(&root).unwrap();
    }

    // Issue #6: editing the H1 of an auto-managed note renames its file to
    // follow, without ever touching the document content.
    #[gpui::test]
    fn editing_an_auto_managed_h1_renames_the_file(cx: &mut gpui::TestAppContext) {
        let root = draft_test_root("h1-rename");
        std::fs::create_dir_all(&root).unwrap();
        let work_folder = OsWorkFolderScanner.scan(&root).unwrap();

        let view = gpui::AppContext::new(cx, |cx| {
            EditorView::from_sessions(
                SessionSet::with_untitled("", "Untitled"),
                Arc::new(OsFileService),
                StateStores::memory(),
                cx,
            )
        });

        view.update(cx, |view, cx| {
            view.work_folder = Some(work_folder);
            view.new_work_folder_note(cx);
            view.editor_mut().insert_text("# LangChain4j").unwrap();
            view.after_input(cx);
        });
        settle_debounce(cx);

        view.update(cx, |view, cx| {
            // Append " Agent" to the H1.
            let end = SourceOffset(view.editor().document().len_bytes().0);
            view.editor_mut()
                .set_selection(Selection::caret(end))
                .unwrap();
            view.editor_mut().insert_text(" Agent").unwrap();
            view.after_input(cx);
        });
        settle_debounce(cx);

        view.read_with(cx, |view, _| {
            assert_eq!(
                view.active_session().path(),
                Some(root.join("LangChain4j Agent.md").as_path())
            );
            assert_eq!(
                view.active_session().auto_title(),
                Some("LangChain4j Agent")
            );
            // The sidebar index must follow the rename too: otherwise it
            // keeps showing the old name (which no longer exists on disk)
            // and drops the new one until the folder is reopened.
            let folder = view.work_folder.as_ref().unwrap();
            assert!(
                folder
                    .entry_for_path(&root.join("LangChain4j.md"))
                    .is_none(),
                "the stale pre-rename path must not linger in the sidebar"
            );
            assert!(
                folder
                    .entry_for_path(&root.join("LangChain4j Agent.md"))
                    .is_some(),
                "the renamed note must be reachable from the sidebar"
            );
        });
        assert!(!root.join("LangChain4j.md").exists());
        assert_eq!(
            std::fs::read_to_string(root.join("LangChain4j Agent.md")).unwrap(),
            "# LangChain4j Agent"
        );

        std::fs::remove_dir_all(&root).unwrap();
    }

    // Issue #6: a file that existed before it was opened must never be
    // auto-renamed just because its H1 is edited — only notes this app
    // itself named from an H1 are auto-managed.
    #[gpui::test]
    fn an_existing_files_h1_is_never_auto_renamed(cx: &mut gpui::TestAppContext) {
        let root = draft_test_root("h1-existing");
        std::fs::create_dir_all(&root).unwrap();
        let existing = root.join("2026-08-29-meeting.md");
        std::fs::write(&existing, "# AI推進室 定例会").unwrap();
        let work_folder = OsWorkFolderScanner.scan(&root).unwrap();

        let view = gpui::AppContext::new(cx, |cx| {
            EditorView::from_sessions(
                SessionSet::with_untitled("", "Untitled"),
                Arc::new(OsFileService),
                StateStores::memory(),
                cx,
            )
        });

        view.update(cx, |view, cx| {
            view.work_folder = Some(work_folder);
            view.open_work_folder_entry(&existing, cx);
        });
        cx.run_until_parked();

        view.update(cx, |view, cx| {
            let end = SourceOffset(view.editor().document().len_bytes().0);
            view.editor_mut()
                .set_selection(Selection::caret(end))
                .unwrap();
            view.editor_mut().insert_text(" 変更").unwrap();
            view.after_input(cx);
        });
        settle_debounce(cx);

        view.read_with(cx, |view, _| {
            assert_eq!(view.active_session().path(), Some(existing.as_path()));
            assert_eq!(view.active_session().auto_title(), None);
        });
        assert!(existing.exists());

        std::fs::remove_dir_all(&root).unwrap();
    }

    // Issue #95: `offset_at_row_x` translated a mouse event's window-space x
    // by only the line's horizontal padding, ignoring that an open
    // work-folder sidebar (and its resizer) shifts the whole main column to
    // the right. Every row hit test was off by that many pixels whenever a
    // sidebar was open, so a mouse-down or drag anywhere in the body text
    // landed on the wrong character. These tests drive the real
    // `EditorView` render tree through `gpui::TestAppContext::simulate_event`
    // and check the resulting selection, instead of calling the handlers
    // directly or testing the coordinate math in isolation.

    /// Predicts the `SourceOffset` a click at `visual_offset` on a row should
    /// land on, and the real window-space point that should produce it — from
    /// the row's actual painted bounds (`debug_selector`) and the same glyph
    /// shaping the renderer used, not from the coordinate translation under
    /// test.
    fn row_click(
        view: &gpui::Entity<EditorView>,
        cx: &mut gpui::VisualTestContext,
        selector: &'static str,
        line: usize,
        row_index: usize,
        visual_offset: usize,
    ) -> (gpui::Point<gpui::Pixels>, SourceOffset) {
        let bounds = cx.debug_bounds(selector).expect("row painted for selector");
        let fragment = row_fragment(view, cx, line, row_index);
        cx.update(|window, app| {
            view.read_with(app, |editor_view, _| {
                let visual = editor_view.rendered_line(line).expect("line rendered");
                let shaper = WindowShaper::new(window, editor_view.zoom);
                let expected = source_offset_for_visual_position(
                    editor_view.editor(),
                    line,
                    &visual,
                    visual_offset,
                    Some(&fragment),
                );
                let (block_id, visual_line) = *editor_view
                    .line_owners
                    .get(&line)
                    .expect("line owner recorded");
                let layout = &editor_view
                    .layout_cache
                    .get(&block_id)
                    .expect("layout cached")
                    .layout;
                let block = editor_view
                    .block_cache
                    .get(&block_id)
                    .expect("block cached");
                let layout_point = layout
                    .point_for_source(block, expected, &shaper)
                    .expect("expected source has a layout point");
                let row = layout
                    .lines
                    .get(layout_point.row)
                    .expect("layout point row exists");
                assert_eq!(row.line, visual_line);
                let x = f32::from(bounds.origin.x)
                    + editor_view.theme.line_horizontal_padding
                    + layout_point.x;
                let y = f32::from(bounds.origin.y) + f32::from(bounds.size.height) / 2.0;
                (point(px(x), px(y)), expected)
            })
        })
    }

    /// The row's own stretch of its line's visual text, as painted this
    /// frame — the same range `on_row_mouse_down`/`on_row_mouse_move` use.
    fn row_fragment(
        view: &gpui::Entity<EditorView>,
        cx: &mut gpui::VisualTestContext,
        line: usize,
        row_index: usize,
    ) -> Range<usize> {
        cx.update(|_, app| {
            view.read_with(app, |editor_view, _| {
                let (block_id, _) = *editor_view
                    .line_owners
                    .get(&line)
                    .expect("line owner recorded");
                editor_view
                    .layout_cache
                    .get(&block_id)
                    .expect("layout cached")
                    .layout
                    .lines[row_index]
                    .line_visual_range
                    .clone()
            })
        })
    }

    /// Opens an `EditorView` in a real window for GPUI mouse-event
    /// regression tests: a fixed size so layout is deterministic, and
    /// optionally a work-folder sidebar so the main column sits to the right
    /// of it — the geometry `on_row_mouse_down`/`on_row_mouse_move` have to
    /// hit-test against. Returns the work folder's temporary root, if any,
    /// for the caller to clean up.
    fn open_view_for_mouse_tests<'a>(
        cx: &'a mut gpui::TestAppContext,
        text: &str,
        with_sidebar: bool,
    ) -> (
        gpui::Entity<EditorView>,
        &'a mut gpui::VisualTestContext,
        Option<PathBuf>,
    ) {
        let text = text.to_owned();
        let (view, cx) = cx.add_window_view(move |_, cx| EditorView::new(&text, "Untitled", cx));
        cx.simulate_resize(gpui::size(px(960.0), px(760.0)));
        cx.run_until_parked();
        let root = if with_sidebar {
            let root = draft_test_root("mouse-drag-sidebar");
            std::fs::create_dir_all(&root).unwrap();
            let work_folder = OsWorkFolderScanner.scan(&root).unwrap();
            view.update(cx, |view, cx| {
                view.work_folder = Some(work_folder);
                cx.notify();
            });
            cx.run_until_parked();
            Some(root)
        } else {
            None
        };
        (view, cx, root)
    }

    fn secondary_scroll_modifiers() -> gpui::Modifiers {
        #[cfg(target_os = "macos")]
        {
            gpui::Modifiers {
                platform: true,
                ..gpui::Modifiers::none()
            }
        }
        #[cfg(not(target_os = "macos"))]
        {
            gpui::Modifiers {
                control: true,
                ..gpui::Modifiers::none()
            }
        }
    }

    // Issue #228: continuous 50%-300% zoom via Ctrl/Cmd+wheel, trackpad
    // pinch, and Ctrl/Cmd+0 reset.

    #[gpui::test]
    fn ctrl_wheel_zooms_while_plain_wheel_only_scrolls(cx: &mut gpui::TestAppContext) {
        let text = (1..=60)
            .map(|n| format!("line {n:02}"))
            .collect::<Vec<_>>()
            .join("\n");
        let (view, cx, _root) = open_view_for_mouse_tests(cx, &text, false);
        let position = point(px(480.0), px(400.0));

        // A plain wheel scrolls the document without touching zoom.
        cx.simulate_event(gpui::ScrollWheelEvent {
            position,
            delta: ScrollDelta::Lines(point(0.0, -5.0)),
            modifiers: gpui::Modifiers::none(),
            touch_phase: gpui::TouchPhase::Moved,
        });
        cx.run_until_parked();
        let (zoom, scroll_y) = view.read_with(cx, |view, _| (view.zoom, view.scroll_y));
        assert_eq!(zoom, 1.0);
        assert!(scroll_y > 0.0, "{scroll_y}");

        // Ctrl+wheel at the same position zooms in instead of scrolling
        // further.
        cx.simulate_event(gpui::ScrollWheelEvent {
            position,
            delta: ScrollDelta::Lines(point(0.0, 5.0)),
            modifiers: secondary_scroll_modifiers(),
            touch_phase: gpui::TouchPhase::Moved,
        });
        cx.run_until_parked();
        let zoomed = view.read_with(cx, |view, _| view.zoom);
        assert!(zoomed > 1.0, "{zoomed}");
    }

    #[gpui::test]
    fn trackpad_pinch_changes_zoom_and_clamps_extreme_input(cx: &mut gpui::TestAppContext) {
        let (view, cx, _root) = open_view_for_mouse_tests(cx, "hello world", false);
        let position = point(px(480.0), px(400.0));

        cx.simulate_event(gpui::MagnifyEvent {
            position,
            magnification: 0.2,
            modifiers: gpui::Modifiers::none(),
            phase: gpui::TouchPhase::Moved,
        });
        cx.run_until_parked();
        let zoomed_in = view.read_with(cx, |view, _| view.zoom);
        assert!((zoomed_in - 1.2).abs() < 1e-4, "{zoomed_in}");

        // A pinch collapsing past zero must clamp instead of going negative
        // or producing a zero/degenerate zoom level.
        cx.simulate_event(gpui::MagnifyEvent {
            position,
            magnification: -5.0,
            modifiers: gpui::Modifiers::none(),
            phase: gpui::TouchPhase::Moved,
        });
        cx.run_until_parked();
        let zoomed_out = view.read_with(cx, |view, _| view.zoom);
        assert_eq!(zoomed_out, MIN_ZOOM);
    }

    #[gpui::test]
    fn small_pinch_deltas_accumulate_before_the_100_percent_snap(cx: &mut gpui::TestAppContext) {
        let (view, cx, _root) = open_view_for_mouse_tests(cx, "hello world", false);
        let position = point(px(480.0), px(400.0));

        for _ in 0..3 {
            cx.simulate_event(gpui::MagnifyEvent {
                position,
                magnification: 0.01,
                modifiers: gpui::Modifiers::none(),
                phase: gpui::TouchPhase::Moved,
            });
            cx.run_until_parked();
        }

        let (zoom, raw_zoom) = view.read_with(cx, |view, _| (view.zoom, view.raw_zoom));
        assert!(
            raw_zoom > 1.02,
            "raw zoom must cross the snap band: {raw_zoom}"
        );
        assert!(
            zoom > 1.0,
            "effective zoom must eventually leave the snap: {zoom}"
        );
    }

    #[gpui::test]
    fn reset_zoom_keystroke_restores_100_percent_after_zooming(cx: &mut gpui::TestAppContext) {
        cx.update(crate::actions::register_key_bindings);
        let (view, cx, _root) = open_view_for_mouse_tests(cx, "hello world", false);

        // A click focuses the editor's "HaneEditor" key context (see
        // `on_row_mouse_down`), which the Ctrl/Cmd+0 binding is scoped to.
        let point = cx
            .debug_bounds("row-0-0")
            .expect("first row painted")
            .center();
        cx.simulate_mouse_down(point, MouseButton::Left, gpui::Modifiers::none());
        cx.simulate_mouse_up(point, MouseButton::Left, gpui::Modifiers::none());

        view.update(cx, |view, cx| {
            view.set_zoom(2.0, view.viewport_height / 2.0, cx);
        });
        cx.run_until_parked();
        assert_eq!(view.read_with(cx, |view, _| view.zoom), 2.0);

        #[cfg(target_os = "macos")]
        cx.simulate_keystrokes("cmd-0");
        #[cfg(not(target_os = "macos"))]
        cx.simulate_keystrokes("ctrl-0");
        cx.run_until_parked();

        assert_eq!(view.read_with(cx, |view, _| view.zoom), 1.0);
    }

    #[gpui::test]
    fn zoom_scales_the_painted_row_height(cx: &mut gpui::TestAppContext) {
        let (view, cx, _root) = open_view_for_mouse_tests(cx, "hello world", false);
        let height_at = |cx: &mut gpui::VisualTestContext, zoom: f32| -> f32 {
            view.update(cx, |view, gpui_cx| view.set_zoom(zoom, 0.0, gpui_cx));
            cx.run_until_parked();
            f32::from(
                cx.debug_bounds("row-0-0")
                    .expect("first row painted")
                    .size
                    .height,
            )
        };

        let height_at_100 = height_at(cx, 1.0);
        let height_at_50 = height_at(cx, MIN_ZOOM);
        let height_at_200 = height_at(cx, 2.0);

        assert!(
            height_at_50 < height_at_100,
            "{height_at_50} < {height_at_100}"
        );
        assert!(
            height_at_200 > height_at_100,
            "{height_at_200} > {height_at_100}"
        );
    }

    #[gpui::test]
    fn clicking_active_draft_after_root_selection_restores_draft_focus(
        cx: &mut gpui::TestAppContext,
    ) {
        let (view, cx, root) = open_view_for_mouse_tests(cx, "", true);
        view.update(cx, |view, cx| view.new_work_folder_note(cx));
        cx.run_until_parked();
        let id = view.read_with(cx, |view, _| view.sessions.active_id());
        let root_point = cx.debug_bounds("sidebar-root").unwrap().center();
        cx.simulate_mouse_down(root_point, MouseButton::Left, gpui::Modifiers::none());
        cx.simulate_mouse_up(root_point, MouseButton::Left, gpui::Modifiers::none());
        view.read_with(cx, |view, _| {
            assert_eq!(view.sidebar_focus, SidebarFocus::Folder)
        });
        cx.run_until_parked();
        let draft_point = cx.debug_bounds("sidebar-draft").unwrap().center();
        cx.simulate_mouse_down(draft_point, MouseButton::Left, gpui::Modifiers::none());
        cx.simulate_mouse_up(draft_point, MouseButton::Left, gpui::Modifiers::none());
        view.read_with(cx, |view, _| {
            assert_eq!(view.sessions.active_id(), id);
            assert_eq!(view.sidebar_focus, SidebarFocus::ActiveSession);
        });
        std::fs::remove_dir_all(root.unwrap()).unwrap();
    }

    #[gpui::test]
    fn drag_selection_from_row_start_lands_on_the_clicked_character_with_sidebar_open(
        cx: &mut gpui::TestAppContext,
    ) {
        let text = "first row of text\n\nsecond paragraph below";
        let (view, cx, root) = open_view_for_mouse_tests(cx, text, true);

        let (down_point, anchor) = row_click(&view, cx, "row-0-0", 0, 0, 0);
        let (move_point, active) = row_click(&view, cx, "row-0-0", 0, 0, 5);
        cx.simulate_mouse_down(down_point, MouseButton::Left, gpui::Modifiers::none());
        cx.simulate_mouse_move(move_point, MouseButton::Left, gpui::Modifiers::none());
        cx.simulate_mouse_up(move_point, MouseButton::Left, gpui::Modifiers::none());
        view.read_with(cx, |view, _| {
            assert_eq!(view.editor().selection(), Selection { anchor, active });
        });
        assert_eq!(anchor, SourceOffset(0));

        // A sidebar resize shifts the main column further right; the same
        // visual offsets must still resolve correctly against the new bounds.
        view.update(cx, |view, cx| {
            view.sidebar_width = 320.0;
            cx.notify();
        });
        cx.run_until_parked();
        let (down_point, anchor) = row_click(&view, cx, "row-0-0", 0, 0, 0);
        let (move_point, active) = row_click(&view, cx, "row-0-0", 0, 0, 5);
        cx.simulate_mouse_down(down_point, MouseButton::Left, gpui::Modifiers::none());
        cx.simulate_mouse_move(move_point, MouseButton::Left, gpui::Modifiers::none());
        cx.simulate_mouse_up(move_point, MouseButton::Left, gpui::Modifiers::none());
        view.read_with(cx, |view, _| {
            assert_eq!(view.editor().selection(), Selection { anchor, active });
        });
        assert_eq!(anchor, SourceOffset(0));

        if let Some(root) = root {
            std::fs::remove_dir_all(root).unwrap();
        }
    }

    #[gpui::test]
    fn drag_selection_from_row_middle_extends_in_either_direction_with_sidebar_open(
        cx: &mut gpui::TestAppContext,
    ) {
        let text = "first row of text\n\nsecond paragraph below";
        let (view, cx, root) = open_view_for_mouse_tests(cx, text, true);

        let (down_point, anchor) = row_click(&view, cx, "row-0-0", 0, 0, 6);
        cx.simulate_mouse_down(down_point, MouseButton::Left, gpui::Modifiers::none());

        let (left_point, left_active) = row_click(&view, cx, "row-0-0", 0, 0, 2);
        cx.simulate_mouse_move(left_point, MouseButton::Left, gpui::Modifiers::none());
        view.read_with(cx, |view, _| {
            assert_eq!(
                view.editor().selection(),
                Selection {
                    anchor,
                    active: left_active
                }
            );
        });
        assert!(left_active < anchor);

        let (right_point, right_active) = row_click(&view, cx, "row-0-0", 0, 0, 10);
        cx.simulate_mouse_move(right_point, MouseButton::Left, gpui::Modifiers::none());
        cx.simulate_mouse_up(right_point, MouseButton::Left, gpui::Modifiers::none());
        view.read_with(cx, |view, _| {
            assert_eq!(
                view.editor().selection(),
                Selection {
                    anchor,
                    active: right_active
                }
            );
        });
        assert!(right_active > anchor);

        if let Some(root) = root {
            std::fs::remove_dir_all(root).unwrap();
        }
    }

    #[gpui::test]
    fn drag_selection_across_lines_keeps_the_original_anchor_with_sidebar_open(
        cx: &mut gpui::TestAppContext,
    ) {
        let text = "first row of text\n\nsecond paragraph below";
        let (view, cx, root) = open_view_for_mouse_tests(cx, text, true);

        let (down_point, anchor) = row_click(&view, cx, "row-0-0", 0, 0, 6);
        cx.simulate_mouse_down(down_point, MouseButton::Left, gpui::Modifiers::none());

        let (move_point, active) = row_click(&view, cx, "row-2-0", 2, 0, 4);
        cx.simulate_mouse_move(move_point, MouseButton::Left, gpui::Modifiers::none());
        cx.simulate_mouse_up(move_point, MouseButton::Left, gpui::Modifiers::none());

        view.read_with(cx, |view, _| {
            assert_eq!(view.editor().selection(), Selection { anchor, active });
        });
        assert_ne!(anchor, active);

        if let Some(root) = root {
            std::fs::remove_dir_all(root).unwrap();
        }
    }

    #[gpui::test]
    fn drag_selection_from_row_start_lands_on_the_clicked_character_without_a_sidebar(
        cx: &mut gpui::TestAppContext,
    ) {
        let text = "first row of text\n\nsecond paragraph below";
        let (view, cx, root) = open_view_for_mouse_tests(cx, text, false);
        assert!(root.is_none());

        let (down_point, anchor) = row_click(&view, cx, "row-0-0", 0, 0, 0);
        let (move_point, active) = row_click(&view, cx, "row-0-0", 0, 0, 5);
        cx.simulate_mouse_down(down_point, MouseButton::Left, gpui::Modifiers::none());
        cx.simulate_mouse_move(move_point, MouseButton::Left, gpui::Modifiers::none());
        cx.simulate_mouse_up(move_point, MouseButton::Left, gpui::Modifiers::none());

        view.read_with(cx, |view, _| {
            assert_eq!(view.editor().selection(), Selection { anchor, active });
        });
        assert_eq!(anchor, SourceOffset(0));
    }

    #[gpui::test]
    fn drag_selection_lands_on_the_second_row_of_a_soft_wrapped_line_with_sidebar_open(
        cx: &mut gpui::TestAppContext,
    ) {
        let text = "wrap word wrap word wrap word wrap word wrap word wrap word";
        let (view, cx, root) = open_view_for_mouse_tests(cx, text, true);
        cx.simulate_resize(gpui::size(px(420.0), px(760.0)));
        cx.run_until_parked();

        let second_row = row_fragment(&view, cx, 0, 1);
        assert!(
            second_row.len() >= 3,
            "line must wrap into a second row with a few characters for this test to be meaningful"
        );
        let start_offset = second_row.start;
        let mid_offset = second_row.start + 3;

        let (down_point, anchor) = row_click(&view, cx, "row-0-1", 0, 1, start_offset);
        let (move_point, active) = row_click(&view, cx, "row-0-1", 0, 1, mid_offset);
        cx.simulate_mouse_down(down_point, MouseButton::Left, gpui::Modifiers::none());
        cx.simulate_mouse_move(move_point, MouseButton::Left, gpui::Modifiers::none());
        cx.simulate_mouse_up(move_point, MouseButton::Left, gpui::Modifiers::none());

        view.read_with(cx, |view, _| {
            assert_eq!(view.editor().selection(), Selection { anchor, active });
        });
        assert_ne!(anchor, active);

        if let Some(root) = root {
            std::fs::remove_dir_all(root).unwrap();
        }
    }

    #[gpui::test]
    fn drag_selection_respects_utf8_character_boundaries_in_japanese_text_with_sidebar_open(
        cx: &mut gpui::TestAppContext,
    ) {
        let text = "あいうえおかきくけこ\n\nsecond paragraph below";
        let (view, cx, root) = open_view_for_mouse_tests(cx, text, true);

        // Each character is 3 UTF-8 bytes; 3 and 9 are the boundaries after
        // the first and third characters.
        let (down_point, anchor) = row_click(&view, cx, "row-0-0", 0, 0, 3);
        let (move_point, active) = row_click(&view, cx, "row-0-0", 0, 0, 9);
        cx.simulate_mouse_down(down_point, MouseButton::Left, gpui::Modifiers::none());
        cx.simulate_mouse_move(move_point, MouseButton::Left, gpui::Modifiers::none());
        cx.simulate_mouse_up(move_point, MouseButton::Left, gpui::Modifiers::none());

        view.read_with(cx, |view, _| {
            assert_eq!(view.editor().selection(), Selection { anchor, active });
        });
        assert_eq!(anchor, SourceOffset(3));
        assert_eq!(active, SourceOffset(9));

        if let Some(root) = root {
            std::fs::remove_dir_all(root).unwrap();
        }
    }

    fn open_inline_rename_test_view<'a>(
        cx: &'a mut gpui::TestAppContext,
        root: &Path,
    ) -> (gpui::Entity<EditorView>, &'a mut gpui::VisualTestContext) {
        cx.update(crate::actions::register_key_bindings);
        let work_folder = OsWorkFolderScanner.scan(root).unwrap();
        let (view, cx) = cx.add_window_view(move |_, cx| {
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
            cx.notify();
        });
        cx.run_until_parked();
        (view, cx)
    }

    #[gpui::test]
    fn sidebar_file_filter_reveals_matches_without_changing_normal_expansion(
        cx: &mut gpui::TestAppContext,
    ) {
        let root = draft_test_root("file-filter-tree");
        let project = root.join("Project");
        let archive = root.join("Archive");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::create_dir_all(&archive).unwrap();
        let target = project.join("Target Note.md");
        std::fs::write(&target, "# Target Note\n").unwrap();
        std::fs::write(project.join("Other.md"), "# Other\n").unwrap();
        std::fs::write(archive.join("Old.md"), "# Old\n").unwrap();
        std::fs::write(root.join("Loose.md"), "# Loose\n").unwrap();
        let (view, cx) = open_inline_rename_test_view(cx, &root);

        view.update(cx, |view, _| {
            view.expanded_folders.insert(archive.clone());
            view.sidebar_scroll.set_offset(point(px(0.0), px(-120.0)));
        });
        cx.run_until_parked();
        let sessions_before = view.read_with(cx, |view, _| view.sessions().count());

        let filter_point = cx.debug_bounds("sidebar-filter").unwrap().center();
        cx.simulate_click(filter_point, gpui::Modifiers::none());
        cx.simulate_input("target note");
        cx.run_until_parked();

        view.read_with(cx, |view, _| {
            assert_eq!(view.sidebar_filter, "target note");
            assert_eq!(view.sidebar_scroll.offset().y, px(0.0));
            assert_eq!(view.sessions().count(), sessions_before);
            assert!(view.expanded_folders.contains(&archive));
            assert!(!view.expanded_folders.contains(&project));

            let mut rows = Vec::new();
            flatten_filtered_work_folder_tree(
                view.work_folder.as_ref().unwrap().children(),
                1,
                &view.sidebar_filter,
                &mut rows,
            );
            let paths: Vec<&Path> = rows.iter().map(|row| row.node.path()).collect();
            assert_eq!(paths, vec![project.as_path(), target.as_path()]);

            let mut folder_name_rows = Vec::new();
            flatten_filtered_work_folder_tree(
                view.work_folder.as_ref().unwrap().children(),
                1,
                "project",
                &mut folder_name_rows,
            );
            assert!(folder_name_rows.is_empty());
        });

        let document_before_undo =
            view.read_with(cx, |view, _| view.editor().document().full_text());
        cx.simulate_keystrokes(if cfg!(target_os = "macos") {
            "cmd-z"
        } else {
            "ctrl-z"
        });
        cx.simulate_keystrokes(if cfg!(target_os = "macos") {
            "cmd-shift-z"
        } else {
            "ctrl-shift-z"
        });
        view.read_with(cx, |view, _| {
            assert_eq!(view.sidebar_filter, "target note");
            assert_eq!(view.editor().document().full_text(), document_before_undo);
        });

        view.update(cx, |view, _| {
            let text = view.sidebar_filter.clone();
            view.sidebar_filter_composition = Some(SidebarFilterComposition {
                text,
                selected_range: view.sidebar_filter_selected_range.clone(),
                selection_reversed: view.sidebar_filter_selection_reversed,
            });
        });
        cx.simulate_keystrokes("enter");
        view.read_with(cx, |view, _| {
            assert!(!view.sidebar_filter_has_composition());
        });

        assert!(cx.debug_bounds("sidebar-filter-empty").is_none());
        assert!(cx.debug_bounds("sidebar-folder").is_some());
        assert!(cx.debug_bounds("sidebar-file").is_some());

        cx.simulate_keystrokes(if cfg!(target_os = "macos") {
            "cmd-a"
        } else {
            "ctrl-a"
        });
        cx.simulate_keystrokes("backspace");
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert!(view.sidebar_filter.is_empty());
            assert!(view.expanded_folders.contains(&archive));
            assert!(!view.expanded_folders.contains(&project));
            let mut rows = Vec::new();
            flatten_work_folder_tree(
                view.work_folder.as_ref().unwrap().children(),
                1,
                &view.expanded_folders,
                &mut rows,
            );
            let paths: Vec<&Path> = rows.iter().map(|row| row.node.path()).collect();
            assert_eq!(
                paths,
                vec![
                    archive.as_path(),
                    archive.join("Old.md").as_path(),
                    root.join("Loose.md").as_path(),
                    project.as_path(),
                ]
            );
        });

        std::fs::remove_dir_all(root).unwrap();
    }

    #[gpui::test]
    fn sidebar_file_filter_keeps_drafts_and_opens_a_visible_match(cx: &mut gpui::TestAppContext) {
        let root = draft_test_root("file-filter-open");
        std::fs::create_dir_all(&root).unwrap();
        let target = root.join("Meeting Notes.md");
        std::fs::write(&target, "# Meeting Notes\n").unwrap();
        let (view, cx) = open_inline_rename_test_view(cx, &root);

        view.update(cx, |view, cx| view.new_work_folder_note(cx));
        cx.run_until_parked();
        let filter_point = cx.debug_bounds("sidebar-filter").unwrap().center();
        cx.simulate_click(filter_point, gpui::Modifiers::none());
        cx.simulate_input("no such note");
        cx.run_until_parked();

        assert!(cx.debug_bounds("sidebar-filter-empty").is_some());
        assert!(cx.debug_bounds("sidebar-draft").is_some());
        view.read_with(cx, |view, _| {
            assert_eq!(view.sessions().count(), 2);
            assert!(view.loading_paths.is_empty());
        });

        cx.simulate_keystrokes(if cfg!(target_os = "macos") {
            "cmd-a"
        } else {
            "ctrl-a"
        });
        cx.simulate_input("MEETING");
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert_eq!(view.sidebar_filter, "MEETING");
            let mut rows = Vec::new();
            flatten_filtered_work_folder_tree(
                view.work_folder.as_ref().unwrap().children(),
                1,
                &view.sidebar_filter.to_lowercase(),
                &mut rows,
            );
            assert_eq!(
                rows.iter().map(|row| row.node.path()).collect::<Vec<_>>(),
                vec![target.as_path()]
            );
        });
        view.update(cx, |view, cx| view.open_work_folder_entry(&target, cx));
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert_eq!(view.active_session().path(), Some(target.as_path()));
            assert_eq!(view.sidebar_filter, "MEETING");
            assert!(!view.sidebar_filter_is_focused());
            let input = view.text_input_render_state().unwrap();
            assert_eq!(input.text, "MEETING");
            assert!(input.selected_range.is_empty());
            assert!(input.marked_range.is_none());
        });

        std::fs::remove_dir_all(root).unwrap();
    }

    fn simulate_double_click(cx: &mut gpui::VisualTestContext, point: gpui::Point<gpui::Pixels>) {
        let modifiers = gpui::Modifiers::none();
        cx.simulate_event(gpui::MouseDownEvent {
            position: point,
            modifiers,
            button: MouseButton::Left,
            click_count: 1,
            first_mouse: false,
        });
        cx.simulate_event(gpui::MouseUpEvent {
            position: point,
            modifiers,
            button: MouseButton::Left,
            click_count: 1,
        });
        cx.simulate_event(gpui::MouseDownEvent {
            position: point,
            modifiers,
            button: MouseButton::Left,
            click_count: 2,
            first_mouse: false,
        });
        cx.simulate_event(gpui::MouseUpEvent {
            position: point,
            modifiers,
            button: MouseButton::Left,
            click_count: 2,
        });
    }

    #[gpui::test]
    fn sidebar_file_f2_escape_and_enter_rename_without_touching_the_extension(
        cx: &mut gpui::TestAppContext,
    ) {
        let root = draft_test_root("inline-rename-file");
        std::fs::create_dir_all(&root).unwrap();
        let original = root.join("Plain.md");
        std::fs::write(&original, "plain").unwrap();
        let (view, cx) = open_inline_rename_test_view(cx, &root);

        let file_point = cx.debug_bounds("sidebar-file").unwrap().center();
        cx.simulate_click(file_point, gpui::Modifiers::none());
        cx.simulate_keystrokes("f2");
        view.read_with(cx, |view, _| {
            assert!(
                view.inline_rename_active(),
                "F2 must open the row-local field"
            );
            assert_eq!(
                view.inline_rename
                    .as_ref()
                    .unwrap()
                    .fixed_extension
                    .as_deref(),
                Some(".md")
            );
        });

        cx.simulate_input("Discarded");
        cx.simulate_keystrokes("escape");
        view.read_with(cx, |view, _| assert!(!view.inline_rename_active()));
        assert!(original.exists());
        assert!(!root.join("Discarded.md").exists());

        cx.simulate_keystrokes("f2");
        cx.simulate_input("Renamed");
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        assert!(!original.exists());
        assert!(root.join("Renamed.md").exists());
        view.read_with(cx, |view, _| {
            assert_eq!(
                view.active_session().path(),
                Some(root.join("Renamed.md").as_path())
            );
            assert!(!view.inline_rename_active());
        });

        std::fs::remove_dir_all(root).unwrap();
    }

    #[gpui::test]
    fn sidebar_folder_f2_starts_inline_rename(cx: &mut gpui::TestAppContext) {
        let root = draft_test_root("inline-rename-folder-f2");
        std::fs::create_dir_all(root.join("Project")).unwrap();
        std::fs::write(root.join("Plain.md"), "plain").unwrap();
        let (view, cx) = open_inline_rename_test_view(cx, &root);

        let folder_point = cx.debug_bounds("sidebar-folder").unwrap().center();
        cx.simulate_click(folder_point, gpui::Modifiers::none());
        cx.simulate_keystrokes("f2");
        view.read_with(cx, |view, _| {
            assert_eq!(
                view.inline_rename.as_ref().map(|rename| rename.kind),
                Some(InlineRenameKind::Folder)
            );
        });

        std::fs::remove_dir_all(root).unwrap();
    }

    #[gpui::test]
    fn clicking_inside_an_inline_rename_moves_the_caret_to_that_text_position(
        cx: &mut gpui::TestAppContext,
    ) {
        let root = draft_test_root("inline-rename-click-caret");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("LongFileName.md"), "plain").unwrap();
        let (view, cx) = open_inline_rename_test_view(cx, &root);

        let file_point = cx.debug_bounds("sidebar-file").unwrap().center();
        cx.simulate_click(file_point, gpui::Modifiers::none());
        cx.simulate_keystrokes("f2");
        cx.run_until_parked();
        let input_bounds = view.read_with(cx, |view, _| {
            assert!(view.inline_rename_active());
            view.inline_rename_input_bounds.unwrap()
        });
        let click = point(input_bounds.left() + px(24.0), input_bounds.center().y);
        cx.simulate_click(click, gpui::Modifiers::none());
        view.read_with(cx, |view, _| {
            let (range, reversed) = view.inline_rename_selection().unwrap();
            assert!(!reversed);
            assert_eq!(range.start, range.end);
            assert!(range.start > 0 && range.start < "LongFileName".encode_utf16().count());
        });

        std::fs::remove_dir_all(root).unwrap();
    }

    #[gpui::test]
    fn sidebar_file_and_folder_double_click_start_inline_rename_but_root_does_not(
        cx: &mut gpui::TestAppContext,
    ) {
        let root = draft_test_root("inline-rename-double-click");
        std::fs::create_dir_all(root.join("Project")).unwrap();
        std::fs::write(root.join("Plain.md"), "plain").unwrap();
        let (view, cx) = open_inline_rename_test_view(cx, &root);

        let file_point = cx.debug_bounds("sidebar-file").unwrap().center();
        simulate_double_click(cx, file_point);
        view.read_with(cx, |view, _| {
            assert_eq!(
                view.inline_rename.as_ref().map(|rename| rename.kind),
                Some(InlineRenameKind::File)
            );
        });
        cx.simulate_keystrokes("escape");

        let folder_point = cx.debug_bounds("sidebar-folder").unwrap().center();
        simulate_double_click(cx, folder_point);
        view.read_with(cx, |view, _| {
            assert_eq!(
                view.inline_rename.as_ref().map(|rename| rename.kind),
                Some(InlineRenameKind::Folder)
            );
        });
        cx.simulate_keystrokes("escape");

        let root_point = cx.debug_bounds("sidebar-root").unwrap().center();
        simulate_double_click(cx, root_point);
        cx.simulate_keystrokes("f2");
        view.read_with(cx, |view, _| {
            assert!(
                !view.inline_rename_active(),
                "the work-folder root is not a rename target"
            );
        });

        std::fs::remove_dir_all(root).unwrap();
    }

    #[gpui::test]
    fn sidebar_click_during_pending_folder_rename_does_not_start_a_load(
        cx: &mut gpui::TestAppContext,
    ) {
        let root = draft_test_root("inline-rename-pending-open");
        let project = root.join("Project");
        let nested = project.join("Nested.md");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(&nested, "nested").unwrap();
        let (view, cx) = open_inline_rename_test_view(cx, &root);

        let folder_point = cx.debug_bounds("sidebar-folder").unwrap().center();
        simulate_double_click(cx, folder_point);
        cx.simulate_input("RenamedProject");
        view.update(cx, |view, _| {
            view.inline_rename
                .as_mut()
                .expect("folder rename input")
                .pending = true;
        });
        view.read_with(cx, |view, _| {
            assert!(
                view.inline_rename
                    .as_ref()
                    .is_some_and(|rename| rename.pending),
                "the filesystem operation must be pending before another row can be clicked"
            );
        });

        let nested_point = cx.debug_bounds("sidebar-file").unwrap().center();
        cx.simulate_click(nested_point, gpui::Modifiers::none());
        view.read_with(cx, |view, _| {
            assert!(
                view.inline_rename
                    .as_ref()
                    .is_some_and(|rename| rename.pending)
            );
            assert!(view.loading_paths.is_empty());
            assert!(view.sessions().all(|session| session.path().is_none()));
        });

        view.update(cx, |view, cx| {
            view.inline_rename
                .as_mut()
                .expect("folder rename input")
                .pending = false;
            view.cancel_inline_rename(cx);
        });
        assert!(root.join("Project/Nested.md").exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[gpui::test]
    fn pending_folder_rename_blocks_opening_an_old_child_path(cx: &mut gpui::TestAppContext) {
        let root = draft_test_root("inline-rename-pending-open");
        let project = root.join("Project");
        let first = project.join("A.md");
        let second = project.join("B.md");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(&first, "a").unwrap();
        std::fs::write(&second, "b").unwrap();
        let (view, cx) = open_inline_rename_test_view(cx, &root);

        view.update(cx, |view, cx| {
            view.inline_rename = Some(InlineRename {
                kind: InlineRenameKind::Folder,
                from: project.clone(),
                text: "RenamedProject".to_owned(),
                fixed_extension: None,
                selected_range: 0..0,
                selection_reversed: false,
                marked_range: None,
                composition: None,
                pending: true,
            });
            view.latest_open_target = None;
            view.open_work_folder_entry(&second, cx);
            assert_eq!(view.latest_open_target, None);
            assert!(!view.loading_paths.contains(&second));
            assert!(
                view.inline_rename
                    .as_ref()
                    .is_some_and(|rename| rename.pending)
            );
        });

        std::fs::remove_dir_all(root).unwrap();
    }

    #[gpui::test]
    fn undo_is_blocked_while_inline_rename_is_active(cx: &mut gpui::TestAppContext) {
        let root = draft_test_root("inline-rename-block-undo");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("Plain.md"), "plain").unwrap();
        let (view, cx) = open_inline_rename_test_view(cx, &root);

        let file_point = cx.debug_bounds("sidebar-file").unwrap().center();
        cx.simulate_click(file_point, gpui::Modifiers::none());
        cx.run_until_parked();

        view.update(cx, |view, cx| {
            let end = SourceOffset(view.editor().document().len_bytes().0);
            view.editor_mut()
                .set_selection(Selection::caret(end))
                .unwrap();
            view.editor_mut().insert_text(" changed").unwrap();
            view.after_input(cx);
        });
        let before = view.read_with(cx, |view, _| view.editor().document().full_text());

        cx.simulate_click(file_point, gpui::Modifiers::none());
        cx.simulate_keystrokes("f2");
        #[cfg(target_os = "macos")]
        cx.simulate_keystrokes("cmd-z");
        #[cfg(not(target_os = "macos"))]
        cx.simulate_keystrokes("ctrl-z");

        view.read_with(cx, |view, _| {
            assert!(view.inline_rename_active());
            assert_eq!(view.editor().document().full_text(), before);
        });
        std::fs::remove_dir_all(root).unwrap();
    }

    #[gpui::test]
    fn folder_rename_moves_open_file_and_save_follows_the_new_path(cx: &mut gpui::TestAppContext) {
        let root = draft_test_root("inline-rename-folder-save");
        let project = root.join("Project");
        let nested = project.join("Nested.md");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(&nested, "before").unwrap();
        let (view, cx) = open_inline_rename_test_view(cx, &root);

        view.update(cx, |view, cx| view.open_work_folder_entry(&nested, cx));
        cx.run_until_parked();
        let folder_point = cx.debug_bounds("sidebar-folder").unwrap().center();
        simulate_double_click(cx, folder_point);
        cx.simulate_input("RenamedProject");
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();

        let renamed_project = root.join("RenamedProject");
        let renamed_nested = renamed_project.join("Nested.md");
        assert!(!project.exists());
        assert!(renamed_nested.exists());
        view.read_with(cx, |view, _| {
            assert_eq!(view.active_session().path(), Some(renamed_nested.as_path()));
            assert_eq!(
                view.selected_folder.as_deref(),
                Some(renamed_project.as_path())
            );
            assert!(view.expanded_folders.contains(&renamed_project));
        });

        view.update(cx, |view, cx| {
            let end = SourceOffset(view.editor().document().len_bytes().0);
            view.editor_mut()
                .set_selection(Selection::caret(end))
                .unwrap();
            view.editor_mut().insert_text(" after").unwrap();
            view.after_input(cx);
            view.save_current(cx);
        });
        cx.run_until_parked();
        assert!(!project.exists());
        assert_eq!(
            std::fs::read_to_string(renamed_nested).unwrap(),
            "before after"
        );

        std::fs::remove_dir_all(root).unwrap();
    }

    #[gpui::test]
    fn folder_rename_waits_for_an_unnamed_drafts_scheduled_title_sync(
        cx: &mut gpui::TestAppContext,
    ) {
        let root = draft_test_root("inline-rename-title-sync-scheduled");
        let project = root.join("Project");
        std::fs::create_dir_all(&project).unwrap();
        let (view, cx) = open_inline_rename_test_view(cx, &root);

        view.update(cx, |view, cx| {
            let id = view.sessions.active_id();
            view.work_folder_drafts.insert(
                id,
                WorkFolderDraft {
                    draft_id: DraftId::generate(),
                    target_directory: project.clone(),
                },
            );
            view.editor_mut().insert_text("# Draft").unwrap();
            view.after_input(cx);

            assert!(view.title_sync_scheduled.contains_key(&id));
            assert!(view.inline_rename_has_background_conflict(&project, InlineRenameKind::Folder));
        });

        std::fs::remove_dir_all(root).unwrap();
    }

    #[gpui::test]
    fn text_autoscroll_uses_the_nearest_edge_when_zones_overlap(cx: &mut gpui::TestAppContext) {
        let view = gpui::AppContext::new(cx, |cx| EditorView::new("text", "Untitled", cx));

        view.update(cx, |view, _| {
            // A short but valid viewport makes the fixed 24px top and bottom
            // edge zones overlap. The pointer must still choose the edge it
            // is actually closest to.
            view.viewport_height = 26.0;
            let header = view.theme.header_height;
            assert_eq!(
                view.text_autoscroll_direction_for(header + 6.0),
                Some(AutoscrollDirection::Up)
            );
            assert_eq!(
                view.text_autoscroll_direction_for(header + 20.0),
                Some(AutoscrollDirection::Down)
            );
        });
    }

    // Issue #213: a text-selection drag held near the editor viewport's top
    // or bottom edge did not autoscroll at all, because `on_row_mouse_move`
    // only fires while the pointer sits over a rendered row and stops firing
    // the instant the drag reaches the edge of what is currently visible.

    #[gpui::test]
    fn drag_selection_autoscrolls_across_soft_wrapped_rows_in_one_source_line(
        cx: &mut gpui::TestAppContext,
    ) {
        let text = "wrap word ".repeat(800);
        let (view, cx, root) = open_view_for_mouse_tests(cx, &text, false);
        assert!(root.is_none());
        cx.simulate_resize(gpui::size(px(420.0), px(760.0)));
        cx.run_until_parked();

        let (down_point, anchor) = row_click(&view, cx, "row-0-0", 0, 0, 0);
        cx.simulate_mouse_down(down_point, MouseButton::Left, gpui::Modifiers::none());
        let (active_before, scroll_before) = view.read_with(cx, |view, _| {
            (view.editor().selection().active, view.scroll_y)
        });
        assert_eq!(active_before, anchor);

        // Start the same timer path as the edge mouse move. The direct ticks
        // below keep this regression test deterministic while still resolving
        // each target through the real WindowShaper-backed visual-row path.
        let activity = cx.update(|window, app| {
            view.update(app, |view, cx| {
                view.set_text_autoscroll(Some(AutoscrollDirection::Down), window, cx);
                view.text_autoscroll_activity
            })
        });
        let ticks = 60;
        for _ in 0..ticks {
            let continued = cx.update(|window, app| {
                view.update(app, |view, cx| {
                    view.step_text_autoscroll(AutoscrollDirection::Down, activity, window, cx)
                })
            });
            assert!(
                continued,
                "soft-wrapped rows must continue before document end"
            );
        }

        let (active_after, scroll_after, active_line) = view.read_with(cx, |view, _| {
            let active = view.editor().selection().active;
            let line = view
                .editor()
                .document()
                .line_for_offset(active)
                .expect("active offset remains valid")
                .0;
            (active, view.scroll_y, line)
        });
        assert!(ticks > 1);
        assert!(active_after > active_before);
        assert_eq!(
            active_line, 0,
            "autoscroll must stay on the one source line"
        );
        assert!(
            scroll_after > scroll_before,
            "visual-row autoscroll should move the viewport: active={active_after:?}, scroll_before={scroll_before}, scroll_after={scroll_after}"
        );

        cx.simulate_mouse_up(down_point, MouseButton::Left, gpui::Modifiers::none());
        view.read_with(cx, |view, _| {
            assert!(!view.text_selection_drag);
            assert_eq!(view.editor().selection().anchor, anchor);
        });
    }

    #[gpui::test]
    fn drag_selection_autoscrolls_down_while_held_near_the_bottom_edge_and_stops_on_mouse_up(
        cx: &mut gpui::TestAppContext,
    ) {
        let text = (0..60)
            .map(|n| format!("line {n}"))
            .collect::<Vec<_>>()
            .join("\n\n");
        let (view, cx, root) = open_view_for_mouse_tests(cx, &text, false);
        assert!(root.is_none());

        let (down_point, anchor) = row_click(&view, cx, "row-0-0", 0, 0, 0);
        cx.simulate_mouse_down(down_point, MouseButton::Left, gpui::Modifiers::none());

        let (header_height, viewport_height) = view.read_with(cx, |view, _| {
            (view.theme.header_height, view.viewport_height)
        });
        let edge_point = point(down_point.x, px(header_height + viewport_height - 4.0));
        cx.simulate_mouse_move(edge_point, MouseButton::Left, gpui::Modifiers::none());

        view.read_with(cx, |view, _| {
            assert_eq!(view.text_autoscroll, Some(AutoscrollDirection::Down));
            assert_eq!(view.editor().selection().anchor, anchor);
        });
        let (active_after_move, scroll_after_move) = view.read_with(cx, |view, _| {
            (view.editor().selection().active, view.scroll_y)
        });

        // The autoscroll loop ticks on a real `gpui::Timer`, the same way the
        // debounce timers `settle_debounce` waits on do: real time has to
        // pass for it to extend the selection and scroll further without the
        // pointer moving again.
        std::thread::sleep(TEXT_SELECTION_AUTOSCROLL_INTERVAL * 3);
        cx.run_until_parked();

        let (active_after_ticks, scroll_after_ticks) = view.read_with(cx, |view, _| {
            (view.editor().selection().active, view.scroll_y)
        });
        assert!(active_after_ticks > active_after_move);
        assert!(scroll_after_ticks > scroll_after_move);

        cx.simulate_mouse_up(edge_point, MouseButton::Left, gpui::Modifiers::none());
        view.read_with(cx, |view, _| {
            assert_eq!(view.text_autoscroll, None);
            assert!(!view.text_selection_drag);
        });

        let active_after_stop = view.read_with(cx, |view, _| view.editor().selection().active);
        std::thread::sleep(TEXT_SELECTION_AUTOSCROLL_INTERVAL * 3);
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert_eq!(view.editor().selection().active, active_after_stop);
        });
    }

    #[gpui::test]
    fn drag_selection_autoscrolls_up_toward_the_document_start_and_then_stops(
        cx: &mut gpui::TestAppContext,
    ) {
        let text = "line 0\n\nline 1\n\nline 2\n\nline 3";
        let (view, cx, root) = open_view_for_mouse_tests(cx, text, false);
        assert!(root.is_none());

        let (down_point, anchor) = row_click(&view, cx, "row-4-0", 4, 0, 0);
        cx.simulate_mouse_down(down_point, MouseButton::Left, gpui::Modifiers::none());

        let header_height = view.read_with(cx, |view, _| view.theme.header_height);
        let edge_point = point(down_point.x, px(header_height - 4.0));
        cx.simulate_mouse_move(edge_point, MouseButton::Left, gpui::Modifiers::none());

        view.read_with(cx, |view, _| {
            assert_eq!(view.text_autoscroll, Some(AutoscrollDirection::Up));
        });

        // Ticks the autoscroll loop's own per-tick logic directly instead of
        // waiting on the real timer that drives it in production: this
        // covers the tick behaviour deterministically without real-time
        // waits — it moves the active edge up one row at a time, keeps the
        // anchor fixed, and stops for good once it reaches the document
        // start instead of spinning uselessly while the pointer stays held
        // past it.
        let activity = view.read_with(cx, |view, _| view.text_autoscroll_activity);
        let mut ticks = 0;
        loop {
            let continued = cx.update(|window, app| {
                view.update(app, |view, cx| {
                    view.step_text_autoscroll(AutoscrollDirection::Up, activity, window, cx)
                })
            });
            ticks += 1;
            if !continued {
                break;
            }
            assert!(
                ticks < 20,
                "should reach the document start well within 20 ticks"
            );
        }
        assert!(ticks > 1);

        view.read_with(cx, |view, _| {
            assert_eq!(view.editor().selection().active, SourceOffset(0));
            assert_eq!(view.editor().selection().anchor, anchor);
            assert_eq!(view.text_autoscroll, None);
        });
    }

    #[gpui::test]
    fn unresolved_mouse_down_does_not_start_text_selection_drag(cx: &mut gpui::TestAppContext) {
        let text = "first row of text\n\nsecond paragraph below";
        let (view, cx, root) = open_view_for_mouse_tests(cx, text, false);
        assert!(root.is_none());

        let (down_point, _) = row_click(&view, cx, "row-0-0", 0, 0, 0);
        let selection_before = view.read_with(cx, |view, _| view.editor().selection());
        view.update(cx, |view, _| {
            // Keep the already-painted row and event handler, but make the
            // current mapping unavailable. This is the same None path caused
            // by a row/cache mismatch during a real frame transition.
            view.layout_cache.clear();
        });
        cx.simulate_mouse_down(down_point, MouseButton::Left, gpui::Modifiers::none());

        view.read_with(cx, |view, _| {
            assert_eq!(view.editor().selection(), selection_before);
            assert!(!view.text_selection_drag);
            assert_eq!(view.text_autoscroll, None);
        });
    }

    #[gpui::test]
    fn document_replacement_cancels_text_selection_autoscroll(cx: &mut gpui::TestAppContext) {
        let text = (0..60)
            .map(|n| format!("line {n}"))
            .collect::<Vec<_>>()
            .join("\n\n");
        let (view, cx, root) = open_view_for_mouse_tests(cx, &text, false);
        assert!(root.is_none());

        let (down_point, _) = row_click(&view, cx, "row-0-0", 0, 0, 0);
        cx.simulate_mouse_down(down_point, MouseButton::Left, gpui::Modifiers::none());
        let (header_height, viewport_height) = view.read_with(cx, |view, _| {
            (view.theme.header_height, view.viewport_height)
        });
        let edge_point = point(down_point.x, px(header_height + viewport_height - 4.0));
        cx.simulate_mouse_move(edge_point, MouseButton::Left, gpui::Modifiers::none());

        let activity = view.read_with(cx, |view, _| {
            assert!(view.text_selection_drag);
            assert_eq!(view.text_autoscroll, Some(AutoscrollDirection::Down));
            view.text_autoscroll_activity
        });

        view.update(cx, |view, cx| {
            view.sessions.open_untitled("replacement", "Replacement");
            view.on_document_replaced();
            cx.notify();
        });

        cx.run_until_parked();
        let (replacement_move, _) = row_click(&view, cx, "row-0-0", 0, 0, 5);
        cx.simulate_mouse_move(replacement_move, MouseButton::Left, gpui::Modifiers::none());

        view.read_with(cx, |view, _| {
            assert!(!view.text_selection_drag);
            assert_eq!(view.text_autoscroll, None);
            assert_ne!(view.text_autoscroll_activity, activity);
            assert_eq!(view.editor().selection(), Selection::caret(SourceOffset(0)));
        });

        // A timer tick that was already queued for the old document must be
        // rejected after the replacement, rather than extending the new one.
        let continued = cx.update(|window, app| {
            view.update(app, |view, cx| {
                view.step_text_autoscroll(AutoscrollDirection::Down, activity, window, cx)
            })
        });
        assert!(!continued);
        view.read_with(cx, |view, _| {
            assert_eq!(view.editor().selection(), Selection::caret(SourceOffset(0)));
        });
    }
}
