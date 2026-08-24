//! GPUI adapter for the editor core. Only visible lines plus bounded overscan are rendered.

use gpui::{
    App, Bounds, Context, Element, ElementId, ElementInputHandler, Entity, EntityInputHandler,
    FocusHandle, Focusable, GlobalElementId, LayoutId, Pixels, ScrollWheelEvent, Style,
    UTF16Selection, Window, actions, div, prelude::*, px, relative, rgb,
};
use hane_document::{Bias, LineCol, LineId, SourceRange, TextBuffer};
use hane_editor::{Editor, EditorCommand};
use hane_presentation::{HeightIndex, StyleKind, present_bold};
use std::ops::Range;
use std::path::Path;
use std::time::{Duration, Instant};

actions!(
    hane_editor,
    [
        Backspace,
        Delete,
        Left,
        Right,
        Up,
        Down,
        SelectLeft,
        SelectRight,
        SelectUp,
        SelectDown,
        SelectAll,
        Home,
        End,
        CancelComposition
    ]
);

const LINE_HEIGHT: f32 = 26.0;
const VIEWPORT_HEIGHT: f32 = 720.0;
const OVERSCAN: f32 = 260.0;

fn line_owns_cursor(
    range: SourceRange,
    cursor: hane_document::SourceOffset,
    is_final_line: bool,
) -> bool {
    range.start <= cursor && (cursor < range.end || (is_final_line && cursor == range.end))
}

pub struct EditorView {
    editor: Editor,
    focus_handle: FocusHandle,
    heights: HeightIndex,
    scroll_y: f32,
    painted_latencies: Vec<Duration>,
    frame_intervals: Vec<Duration>,
    layout_latencies: Vec<Duration>,
    last_paint_at: Option<Instant>,
    file_label: String,
}

impl EditorView {
    pub fn new(text: &str, file_label: impl Into<String>, cx: &mut Context<Self>) -> Self {
        let editor = Editor::new(text);
        let heights = HeightIndex::new(std::iter::repeat_n(
            LINE_HEIGHT,
            editor.document().line_count(),
        ));
        Self {
            editor,
            focus_handle: cx.focus_handle(),
            heights,
            scroll_y: 0.0,
            painted_latencies: Vec::new(),
            frame_intervals: Vec::new(),
            layout_latencies: Vec::new(),
            last_paint_at: None,
            file_label: file_label.into(),
        }
    }

    pub fn open(path: &Path, cx: &mut Context<Self>) -> std::io::Result<Self> {
        let text = std::fs::read_to_string(path)?;
        Ok(Self::new(&text, path.display().to_string(), cx))
    }

    pub fn editor(&self) -> &Editor {
        &self.editor
    }

    #[cfg(debug_assertions)]
    pub fn set_cursor_offset_for_development(
        &mut self,
        offset: usize,
        cx: &mut Context<Self>,
    ) -> Result<(), hane_document::BufferError> {
        self.editor
            .set_selection(hane_editor::Selection::caret(hane_document::SourceOffset(
                offset,
            )))?;
        self.after_input(cx);
        Ok(())
    }

    fn after_input(&mut self, cx: &mut Context<Self>) {
        let lines = self.editor.document().line_count();
        if lines != self.heights.len() {
            self.heights = HeightIndex::new(std::iter::repeat_n(LINE_HEIGHT, lines));
        }
        self.scroll_cursor_into_view();
        cx.notify();
    }

    fn dispatch(&mut self, command: EditorCommand<'_>, cx: &mut Context<Self>) {
        if let Err(error) = self.editor.dispatch(command) {
            eprintln!("editor command rejected: {error}");
        }
        self.after_input(cx);
    }

    fn backspace(&mut self, _: &Backspace, _: &mut Window, cx: &mut Context<Self>) {
        self.dispatch(EditorCommand::Backspace, cx);
    }
    fn delete(&mut self, _: &Delete, _: &mut Window, cx: &mut Context<Self>) {
        self.dispatch(EditorCommand::Delete, cx);
    }
    fn left(&mut self, _: &Left, _: &mut Window, cx: &mut Context<Self>) {
        self.dispatch(EditorCommand::MoveLeft { extend: false }, cx);
    }
    fn right(&mut self, _: &Right, _: &mut Window, cx: &mut Context<Self>) {
        self.dispatch(EditorCommand::MoveRight { extend: false }, cx);
    }
    fn up(&mut self, _: &Up, _: &mut Window, cx: &mut Context<Self>) {
        self.dispatch(EditorCommand::MoveUp { extend: false }, cx);
    }
    fn down(&mut self, _: &Down, _: &mut Window, cx: &mut Context<Self>) {
        self.dispatch(EditorCommand::MoveDown { extend: false }, cx);
    }
    fn select_left(&mut self, _: &SelectLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.dispatch(EditorCommand::MoveLeft { extend: true }, cx);
    }
    fn select_right(&mut self, _: &SelectRight, _: &mut Window, cx: &mut Context<Self>) {
        self.dispatch(EditorCommand::MoveRight { extend: true }, cx);
    }
    fn select_up(&mut self, _: &SelectUp, _: &mut Window, cx: &mut Context<Self>) {
        self.dispatch(EditorCommand::MoveUp { extend: true }, cx);
    }
    fn select_down(&mut self, _: &SelectDown, _: &mut Window, cx: &mut Context<Self>) {
        self.dispatch(EditorCommand::MoveDown { extend: true }, cx);
    }
    fn select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        self.dispatch(EditorCommand::SelectAll, cx);
    }
    fn home(&mut self, _: &Home, _: &mut Window, cx: &mut Context<Self>) {
        self.dispatch(EditorCommand::MoveToStart { extend: false }, cx);
    }
    fn end(&mut self, _: &End, _: &mut Window, cx: &mut Context<Self>) {
        self.dispatch(EditorCommand::MoveToEnd { extend: false }, cx);
    }
    fn cancel_composition(
        &mut self,
        _: &CancelComposition,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let _ = self.editor.cancel_composition();
        self.after_input(cx);
    }

    fn on_scroll(&mut self, event: &ScrollWheelEvent, _: &mut Window, cx: &mut Context<Self>) {
        let delta = event.delta.pixel_delta(px(LINE_HEIGHT));
        let max = (self.heights.total_height() - VIEWPORT_HEIGHT).max(0.0);
        self.scroll_y = (self.scroll_y - f32::from(delta.y)).clamp(0.0, max);
        cx.notify();
    }

    fn scroll_cursor_into_view(&mut self) {
        let Ok(line) = self
            .editor
            .document()
            .line_for_offset(self.editor.selection().active)
        else {
            return;
        };
        let top = self.heights.prefix_sum(line.0);
        if top < self.scroll_y {
            self.scroll_y = top;
        } else if top + LINE_HEIGHT > self.scroll_y + VIEWPORT_HEIGHT {
            self.scroll_y = top + LINE_HEIGHT - VIEWPORT_HEIGHT;
        }
    }

    fn line_range(&self, line: usize) -> SourceRange {
        let start = self
            .editor
            .document()
            .offset_for_line_col(LineId(line), LineCol(0))
            .unwrap()
            .0;
        let end = if line + 1 < self.editor.document().line_count() {
            self.editor
                .document()
                .offset_for_line_col(LineId(line + 1), LineCol(0))
                .unwrap()
                .0
        } else {
            self.editor.document().len_bytes().0
        };
        SourceRange::new(start, end)
    }

    fn line_element(&self, line: usize) -> gpui::Div {
        let range = self.line_range(line);
        let source = self.editor.document().text(range).unwrap_or_default();
        let mut block = present_bold(
            line as u64,
            self.editor.document().revision(),
            range,
            &source,
        );
        while block.visual_text.ends_with(['\r', '\n']) {
            block.visual_text.pop();
        }
        let cursor = self.editor.selection().active;
        let visual_cursor = if line_owns_cursor(
            range,
            cursor,
            line + 1 == self.editor.document().line_count(),
        ) {
            block
                .source_map
                .source_to_visual(cursor, Bias::After)
                .map(|c| c.visual_offset.0)
                .or_else(|| (cursor == range.start).then_some(0))
        } else {
            None
        };
        let visual_cursor = visual_cursor.map(|at| at.min(block.visual_text.len()));
        let mut boundaries = vec![0, block.visual_text.len()];
        boundaries.extend(visual_cursor);
        for run in &block.style_runs {
            boundaries.push(run.visual_range.start.0);
            boundaries.push(run.visual_range.end.0);
        }
        boundaries.sort_unstable();
        boundaries.dedup();
        let mut spans = Vec::new();
        for pair in boundaries.windows(2) {
            let (start, end) = (pair[0], pair[1]);
            if visual_cursor == Some(start) {
                spans.push(cursor_overlay().into_any_element());
            }
            if start == end
                || !block.visual_text.is_char_boundary(start)
                || !block.visual_text.is_char_boundary(end)
            {
                continue;
            }
            let bold = block.style_runs.iter().any(|run| {
                run.kind == StyleKind::Bold
                    && start >= run.visual_range.start.0
                    && end <= run.visual_range.end.0
            });
            spans.push(
                div()
                    .when(bold, |d| d.font_weight(gpui::FontWeight::BOLD))
                    .child(block.visual_text[start..end].to_owned())
                    .into_any_element(),
            );
        }
        if visual_cursor == Some(block.visual_text.len()) {
            spans.push(cursor_overlay().into_any_element());
        }
        let selected = self.editor.selection().range().intersects(range);
        div()
            .h(px(LINE_HEIGHT))
            .w_full()
            .flex()
            .items_center()
            .px_3()
            .when(selected, |d| d.bg(rgb(0xe8eefc)))
            .children(spans)
    }
}

fn cursor_overlay() -> gpui::Div {
    div()
        .relative()
        .flex_none()
        .w(px(0.))
        .h(px(LINE_HEIGHT))
        .child(
            div()
                .absolute()
                .top(px(3.))
                .left(px(0.))
                .w(px(1.))
                .h(px(LINE_HEIGHT - 6.))
                .bg(rgb(0x262626)),
        )
}

impl Focusable for EditorView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl EntityInputHandler for EditorView {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        actual_range: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let (text, actual) = self.editor.text_for_utf16_range(range_utf16).ok()?;
        actual_range.replace(actual);
        Some(text)
    }
    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self
                .editor
                .source_range_to_utf16(self.editor.selection().range())
                .ok()?,
            reversed: self.editor.selection().is_reversed(),
        })
    }
    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.editor
            .ime()
            .and_then(|ime| self.editor.source_range_to_utf16(ime.current_range).ok())
    }
    fn unmark_text(&mut self, _: &mut Window, _: &mut Context<Self>) {
        self.editor.commit_composition();
    }
    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let result = if range_utf16.is_none() && self.editor.ime().is_none() {
            self.editor.insert_text(new_text).map(|_| ())
        } else {
            self.editor.commit_text(range_utf16, new_text).map(|_| ())
        };
        if let Err(error) = result {
            eprintln!("text input rejected: {error}");
        }
        self.after_input(cx);
    }
    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selected_range_utf16: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Err(error) =
            self.editor
                .replace_and_mark_text(range_utf16, new_text, new_selected_range_utf16)
        {
            eprintln!("IME update rejected: {error}");
        }
        self.after_input(cx);
    }
    fn bounds_for_range(
        &mut self,
        _: Range<usize>,
        bounds: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        Some(bounds)
    }
    fn character_index_for_point(
        &mut self,
        _: gpui::Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        self.editor
            .source_range_to_utf16(SourceRange::empty(self.editor.selection().active.0))
            .ok()
            .map(|r| r.start)
    }
}

struct InputCapture {
    input: Entity<EditorView>,
}
impl IntoElement for InputCapture {
    type Element = Self;
    fn into_element(self) -> Self::Element {
        self
    }
}
impl Element for InputCapture {
    type RequestLayoutState = ();
    type PrepaintState = ();
    fn id(&self) -> Option<ElementId> {
        None
    }
    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }
    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = px(1.).into();
        (window.request_layout(style, [], cx), ())
    }
    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        _: &mut Window,
        _: &mut App,
    ) {
    }
    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        let focus = self.input.read(cx).focus_handle.clone();
        window.handle_input(
            &focus,
            ElementInputHandler::new(bounds, self.input.clone()),
            cx,
        );
        self.input.update(cx, |view, _| {
            let now = Instant::now();
            if let Some(previous) = view.last_paint_at.replace(now) {
                view.frame_intervals.push(now.duration_since(previous));
            }
            view.painted_latencies.extend(
                view.editor
                    .mark_frame_painted()
                    .into_iter()
                    .filter_map(|m| m.keystroke_to_frame()),
            );
            if view.frame_intervals.len() > 4_096 {
                view.frame_intervals.drain(..2_048);
            }
            if view.painted_latencies.len() > 4_096 {
                view.painted_latencies.drain(..2_048);
            }
        });
    }
}

impl Render for EditorView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let layout_started = Instant::now();
        let visible = self
            .heights
            .visible_range(self.scroll_y, VIEWPORT_HEIGHT, OVERSCAN);
        let top_space = self.heights.prefix_sum(visible.start);
        let bottom_space =
            (self.heights.total_height() - self.heights.prefix_sum(visible.end)).max(0.0);
        let revision = self.editor.document().revision().0;
        let bytes = self.editor.document().len_bytes().0;
        let p95 = if self.painted_latencies.is_empty() {
            0.0
        } else {
            let mut values = self.painted_latencies.clone();
            values.sort_unstable();
            values[((values.len() as f64 * 0.95).ceil() as usize).saturating_sub(1)].as_secs_f64()
                * 1000.0
        };
        let root = div()
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(0xfaf9f7))
            .text_color(rgb(0x262626))
            .key_context("HaneEditor")
            .track_focus(&self.focus_handle(cx))
            .on_action(cx.listener(Self::backspace))
            .on_action(cx.listener(Self::delete))
            .on_action(cx.listener(Self::left))
            .on_action(cx.listener(Self::right))
            .on_action(cx.listener(Self::up))
            .on_action(cx.listener(Self::down))
            .on_action(cx.listener(Self::select_left))
            .on_action(cx.listener(Self::select_right))
            .on_action(cx.listener(Self::select_up))
            .on_action(cx.listener(Self::select_down))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::home))
            .on_action(cx.listener(Self::end))
            .on_action(cx.listener(Self::cancel_composition))
            .on_scroll_wheel(cx.listener(Self::on_scroll))
            .child(
                div()
                    .h(px(38.))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_between()
                    .px_3()
                    .bg(rgb(0x242424))
                    .text_color(rgb(0xf5f5f5))
                    .child(self.file_label.clone())
                    .child(format!(
                        "rev {revision} · {bytes} bytes · frame p95 {p95:.2} ms"
                    )),
            );
        self.layout_latencies.push(layout_started.elapsed());
        if self.layout_latencies.len() > 4_096 {
            self.layout_latencies.drain(..2_048);
        }
        root.child(
            div()
                .relative()
                .flex_1()
                .overflow_hidden()
                .child(InputCapture { input: cx.entity() })
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .w_full()
                        .child(div().h(px(top_space)))
                        .children(visible.map(|line| self.line_element(line)))
                        .child(div().h(px(bottom_space))),
                ),
        )
    }
}

pub fn register_key_bindings(cx: &mut App) {
    cx.bind_keys([
        gpui::KeyBinding::new("backspace", Backspace, Some("HaneEditor")),
        gpui::KeyBinding::new("delete", Delete, Some("HaneEditor")),
        gpui::KeyBinding::new("left", Left, Some("HaneEditor")),
        gpui::KeyBinding::new("right", Right, Some("HaneEditor")),
        gpui::KeyBinding::new("up", Up, Some("HaneEditor")),
        gpui::KeyBinding::new("down", Down, Some("HaneEditor")),
        gpui::KeyBinding::new("shift-left", SelectLeft, Some("HaneEditor")),
        gpui::KeyBinding::new("shift-right", SelectRight, Some("HaneEditor")),
        gpui::KeyBinding::new("shift-up", SelectUp, Some("HaneEditor")),
        gpui::KeyBinding::new("shift-down", SelectDown, Some("HaneEditor")),
        gpui::KeyBinding::new("cmd-a", SelectAll, Some("HaneEditor")),
        gpui::KeyBinding::new("home", Home, Some("HaneEditor")),
        gpui::KeyBinding::new("end", End, Some("HaneEditor")),
        gpui::KeyBinding::new("escape", CancelComposition, Some("HaneEditor")),
    ]);
}

#[cfg(test)]
mod tests {
    use super::*;
    use hane_document::SourceOffset;

    #[test]
    fn shared_line_boundary_belongs_only_to_the_following_line() {
        let first = SourceRange::new(0, 4);
        let second = SourceRange::new(4, 8);
        let cursor = SourceOffset(4);

        assert!(!line_owns_cursor(first, cursor, false));
        assert!(line_owns_cursor(second, cursor, true));
    }

    #[test]
    fn document_end_belongs_to_the_final_line() {
        assert!(line_owns_cursor(
            SourceRange::new(4, 8),
            SourceOffset(8),
            true
        ));
    }

    #[test]
    fn trailing_empty_line_exclusively_owns_document_end() {
        let content_line = SourceRange::new(0, 4);
        let trailing_empty_line = SourceRange::new(4, 4);
        let cursor = SourceOffset(4);

        assert!(!line_owns_cursor(content_line, cursor, false));
        assert!(line_owns_cursor(trailing_empty_line, cursor, true));
    }
}
