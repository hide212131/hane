use crate::actions::install_action_listeners;
use crate::capture::InputCapture;
use crate::line::{line_element_from_block, presented_line};
use crate::phase0_metrics::Phase0MetricsOutput;
use crate::theme::{DEFAULT_THEME, Theme};
use gpui::{
    App, Context, FocusHandle, Focusable, InteractiveElement, IntoElement, MouseButton,
    MouseDownEvent, MouseMoveEvent, ParentElement, Render, ScrollWheelEvent, Styled, TextRun,
    Window, div, px, rgb,
};
use hane_document::{
    Bias, BufferError, LineId, RevisionDelta, RopeBuffer, SourceOffset, SourceRange, TextBuffer,
};
use hane_editor::{Editor, EditorCommand, Selection};
use hane_metrics::FrameMetrics;
use hane_presentation::{HeightIndex, VisualOffset};
use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, Instant};

const METRICS_CAPACITY: usize = 4_096;

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

pub struct EditorView {
    pub(crate) editor: Editor,
    pub(crate) focus_handle: FocusHandle,
    heights: HeightIndex,
    scroll_y: f32,
    viewport_height: f32,
    pub(crate) metrics: FrameMetrics,
    file_label: String,
    status: Option<String>,
    theme: Theme,
    pub(crate) process_started: Instant,
    pub(crate) file_open_time: Duration,
    pub(crate) load_rss_bytes: Option<u64>,
    pub(crate) ready_reported: bool,
    pub(crate) ready_armed: bool,
    pub(crate) metrics_output: Option<Phase0MetricsOutput>,
    background_presentation_generation: u64,
    line_cache: HashMap<usize, hane_presentation::VisualBlock>,
    display_linked_scroll_direction: Option<f32>,
}

impl EditorView {
    pub fn new(text: &str, file_label: impl Into<String>, cx: &mut Context<Self>) -> Self {
        Self::from_editor(Editor::new(text), file_label, cx)
    }

    fn from_editor(editor: Editor, file_label: impl Into<String>, cx: &mut Context<Self>) -> Self {
        let theme = DEFAULT_THEME;
        let heights = HeightIndex::new(std::iter::repeat_n(
            theme.line_height,
            editor.document().line_count(),
        ));
        Self {
            editor,
            focus_handle: cx.focus_handle(),
            heights,
            scroll_y: 0.0,
            viewport_height: theme.line_height,
            metrics: FrameMetrics::new(METRICS_CAPACITY),
            file_label: file_label.into(),
            status: None,
            theme,
            process_started: Instant::now(),
            file_open_time: Duration::ZERO,
            load_rss_bytes: None,
            ready_reported: false,
            ready_armed: false,
            metrics_output: Phase0MetricsOutput::from_environment().unwrap_or_else(|error| {
                eprintln!("could not open HANE_METRICS_CSV: {error}");
                None
            }),
            background_presentation_generation: 0,
            line_cache: HashMap::new(),
            display_linked_scroll_direction: None,
        }
    }

    pub fn open(path: &Path, cx: &mut Context<Self>) -> std::io::Result<Self> {
        let started = Instant::now();
        let file = std::fs::File::open(path)?;
        let document = RopeBuffer::from_reader(std::io::BufReader::new(file))?;
        let mut view = Self::from_editor(
            Editor::from_document(document),
            path.display().to_string(),
            cx,
        );
        view.file_open_time = started.elapsed();
        view.load_rss_bytes = process_rss_bytes();
        Ok(view)
    }

    pub fn editor(&self) -> &Editor {
        &self.editor
    }

    pub fn arm_startup_timing(&mut self, process_started: Instant) {
        self.process_started = process_started;
        self.ready_armed = true;
    }

    pub fn record_phase0_idle_memory(&mut self, rss_bytes: Option<u64>) {
        if let Some(output) = &mut self.metrics_output
            && let Err(error) = output.memory("memory_idle_30s", rss_bytes)
        {
            eprintln!("could not write idle memory metrics: {error}");
        }
    }

    pub fn apply_phase0_background_presentation(
        &mut self,
        generation: u64,
        cx: &mut Context<Self>,
    ) {
        self.background_presentation_generation = generation;
        cx.notify();
    }

    pub fn enable_display_linked_scroll_measurement(&mut self) {
        self.display_linked_scroll_direction = Some(1.0);
    }

    fn apply_phase1_scroll_frame(&mut self) {
        let direction = self.display_linked_scroll_direction.unwrap_or(1.0);
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
        self.display_linked_scroll_direction = Some(next_direction);
    }

    pub fn set_cursor_offset_for_measurement(
        &mut self,
        offset: usize,
        cx: &mut Context<Self>,
    ) -> Result<(), BufferError> {
        self.editor
            .set_selection(hane_editor::Selection::caret(hane_document::SourceOffset(
                offset,
            )))?;
        self.after_input(cx);
        Ok(())
    }

    pub fn move_cursor_down_for_development(
        &mut self,
        count: usize,
        cx: &mut Context<Self>,
    ) -> Result<(), BufferError> {
        for _ in 0..count {
            self.editor
                .dispatch(EditorCommand::MoveDown { extend: false })?;
        }
        self.after_input(cx);
        Ok(())
    }

    pub(crate) fn after_input(&mut self, cx: &mut Context<Self>) {
        let lines = self.editor.document().line_count();
        if lines != self.heights.len() {
            self.heights = HeightIndex::new(std::iter::repeat_n(self.theme.line_height, lines));
            self.line_cache.clear();
        }
        self.scroll_cursor_into_view();
        cx.notify();
    }

    pub(crate) fn report_error(&mut self, operation: &str, error: BufferError) {
        self.status = Some(format!("{operation} rejected: {error}"));
    }

    pub(crate) fn dispatch(&mut self, command: EditorCommand<'_>, cx: &mut Context<Self>) {
        match self.editor.dispatch(command) {
            Ok(_) => self.status = None,
            Err(error) => self.report_error("editor command", error),
        }
        self.after_input(cx);
    }

    pub(crate) fn perform_cancel_composition(&mut self, cx: &mut Context<Self>) {
        if let Err(error) = self.editor.cancel_composition() {
            self.report_error("composition cancel", error);
        }
        self.after_input(cx);
    }

    fn on_scroll(&mut self, event: &ScrollWheelEvent, _: &mut Window, cx: &mut Context<Self>) {
        let delta = event.delta.pixel_delta(px(self.theme.line_height));
        let max = (self.heights.total_height() - self.viewport_height).max(0.0);
        self.scroll_y = (self.scroll_y - f32::from(delta.y)).clamp(0.0, max);
        cx.notify();
    }

    fn on_line_mouse_down(
        &mut self,
        line: usize,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus_handle);
        let Some(block) = presented_line(&self.editor, line) else {
            return;
        };
        let visual_offset = visual_offset_at_x(&block, event.position.x, self.theme, window);
        let offset = source_offset_for_visual_position(&self.editor, line, &block, visual_offset);
        let selection = if event.modifiers.shift {
            Selection {
                anchor: self.editor.selection().anchor,
                active: offset,
            }
        } else {
            Selection::caret(offset)
        };
        if let Err(error) = self.editor.set_selection(selection) {
            self.report_error("mouse selection", error);
        } else {
            self.status = None;
        }
        self.after_input(cx);
    }

    fn on_line_mouse_move(
        &mut self,
        line: usize,
        event: &MouseMoveEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !event.dragging() {
            return;
        }
        let Some(block) = presented_line(&self.editor, line) else {
            return;
        };
        let visual = visual_offset_at_x(&block, event.position.x, self.theme, window);
        let offset = source_offset_for_visual_position(&self.editor, line, &block, visual);
        let selection = Selection {
            anchor: self.editor.selection().anchor,
            active: offset,
        };
        if self.editor.set_selection(selection).is_ok() {
            self.after_input(cx);
        }
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
        self.scroll_y = scroll_y_for_cursor(
            self.scroll_y,
            top,
            self.theme.line_height,
            self.viewport_height,
        );
    }

    fn cached_line(&mut self, line: usize) -> Option<hane_presentation::VisualBlock> {
        let current_revision = self.editor.document().revision();
        if let Some(mut block) = self.line_cache.remove(&line) {
            let reusable = if block.revision == current_revision {
                true
            } else if let Ok(deltas) = self.editor.document().deltas_since(block.revision) {
                !deltas
                    .iter()
                    .any(|delta| edit_affects_range(*delta, block.source_range))
                    && block.rebase(&deltas, current_revision)
                    && self
                        .editor
                        .document()
                        .line_range(LineId(line))
                        .is_ok_and(|range| range == block.source_range)
            } else {
                false
            };
            if reusable {
                self.line_cache.insert(line, block.clone());
                return Some(block);
            }
        }
        let block = presented_line(&self.editor, line)?;
        self.line_cache.insert(line, block.clone());
        Some(block)
    }
}

fn edit_affects_range(delta: RevisionDelta, range: SourceRange) -> bool {
    let edit = delta.edited_source_range_before;
    if edit.is_empty() {
        range.start <= edit.start && edit.start <= range.end
    } else {
        range.intersects(edit)
    }
}

fn process_rss_bytes() -> Option<u64> {
    hane_metrics::process_memory_bytes()
}

impl Focusable for EditorView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for EditorView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let layout_started = Instant::now();
        self.viewport_height = (f32::from(window.viewport_size().height)
            - self.theme.header_height)
            .max(self.theme.line_height);
        if self.display_linked_scroll_direction.is_some() {
            self.apply_phase1_scroll_frame();
            window.request_animation_frame();
        }
        let max_scroll = (self.heights.total_height() - self.viewport_height).max(0.0);
        self.scroll_y = self.scroll_y.clamp(0.0, max_scroll);
        let visible =
            self.heights
                .visible_range(self.scroll_y, self.viewport_height, self.theme.overscan);
        let cache_start = visible.start.saturating_sub(128);
        let cache_end = (visible.end + 128).min(self.heights.len());
        self.line_cache
            .retain(|line, _| cache_start <= *line && *line < cache_end);
        let rendered_lines = visible
            .clone()
            .filter_map(|line| self.cached_line(line).map(|block| (line, block)))
            .collect::<Vec<_>>();
        let top_space = self.heights.prefix_sum(visible.start);
        let bottom_space =
            (self.heights.total_height() - self.heights.prefix_sum(visible.end)).max(0.0);
        let revision = self.editor.document().revision().0;
        let bytes = self.editor.document().len_bytes().0;
        let p95 = self
            .metrics
            .painted_percentile(0.95)
            .map_or(0.0, |duration| duration.as_secs_f64() * 1000.0);
        let background = if self.background_presentation_generation > 0 {
            format!(" · bg {}", self.background_presentation_generation)
        } else {
            String::new()
        };
        let status = self.status.clone().unwrap_or_else(|| {
            format!("rev {revision} · {bytes} bytes · frame p95 {p95:.2} ms{background}")
        });

        let root = div()
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(self.theme.editor_background))
            .text_color(rgb(self.theme.foreground))
            .key_context("HaneEditor")
            .track_focus(&self.focus_handle(cx));
        let root = install_action_listeners(root, cx)
            .on_scroll_wheel(cx.listener(Self::on_scroll))
            .child(
                div()
                    .h(px(self.theme.header_height))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_between()
                    .px_3()
                    .bg(rgb(self.theme.header_background))
                    .text_color(rgb(self.theme.header_foreground))
                    .child(self.file_label.clone())
                    .child(status),
            );
        let rendered = root.child(
            div()
                .relative()
                .flex_1()
                .overflow_hidden()
                .child(InputCapture { input: cx.entity() })
                .child(
                    div()
                        .absolute()
                        .top(px(content_top_for_scroll(self.scroll_y)))
                        .flex()
                        .flex_col()
                        .w_full()
                        .child(div().h(px(top_space)))
                        .children(rendered_lines.into_iter().map(|(line, block)| {
                            line_element_from_block(&self.editor, line, &block, self.theme)
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(move |view, event, window, cx| {
                                        view.on_line_mouse_down(line, event, window, cx)
                                    }),
                                )
                                .on_mouse_move(cx.listener(move |view, event, window, cx| {
                                    view.on_line_mouse_move(line, event, window, cx)
                                }))
                        }))
                        .child(div().h(px(bottom_space))),
                ),
        );
        self.metrics.record_layout(layout_started.elapsed());
        rendered
    }
}

fn visual_offset_at_x(
    block: &hane_presentation::VisualBlock,
    position_x: gpui::Pixels,
    theme: Theme,
    window: &mut Window,
) -> usize {
    if block.visual_text.is_empty() {
        return 0;
    }
    let style = window.text_style();
    let run = TextRun {
        len: block.visual_text.len(),
        font: style.font(),
        color: style.color,
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    let font_size = style.font_size.to_pixels(window.rem_size());
    let layout =
        window
            .text_system()
            .shape_line(block.visual_text.clone().into(), font_size, &[run], None);
    layout.closest_index_for_x(position_x - px(theme.line_horizontal_padding))
}

fn source_offset_for_visual_position(
    editor: &Editor,
    line: usize,
    block: &hane_presentation::VisualBlock,
    visual_offset: usize,
) -> SourceOffset {
    block
        .source_map
        .visual_to_source(VisualOffset(visual_offset), Bias::After)
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

#[cfg(test)]
mod tests {
    use super::*;
    use hane_document::LineId;

    #[test]
    fn visual_click_positions_map_back_to_source_offsets() {
        let editor = Editor::new("ab🙂\n\n**bold**");

        let first = presented_line(&editor, 0).unwrap();
        assert_eq!(
            source_offset_for_visual_position(&editor, 0, &first, 2),
            SourceOffset(2)
        );
        assert_eq!(
            source_offset_for_visual_position(&editor, 0, &first, first.visual_text.len()),
            SourceOffset(6)
        );

        let empty = presented_line(&editor, 1).unwrap();
        assert_eq!(
            source_offset_for_visual_position(&editor, 1, &empty, 0),
            SourceOffset(7)
        );

        let bold = presented_line(&editor, 2).unwrap();
        assert_eq!(
            source_offset_for_visual_position(&editor, 2, &bold, 0),
            SourceOffset(8)
        );
        assert_eq!(
            source_offset_for_visual_position(&editor, 2, &bold, bold.visual_text.len()),
            SourceOffset(16)
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
}
