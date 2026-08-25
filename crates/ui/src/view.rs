use crate::actions::install_action_listeners;
use crate::capture::InputCapture;
use crate::line::line_element;
use crate::theme::{DEFAULT_THEME, Theme};
use gpui::{
    App, Context, FocusHandle, Focusable, InteractiveElement, IntoElement, ParentElement, Render,
    ScrollWheelEvent, Styled, Window, div, px, rgb,
};
use hane_document::{BufferError, TextBuffer};
use hane_editor::{Editor, EditorCommand};
use hane_metrics::FrameMetrics;
use hane_presentation::HeightIndex;
use std::path::Path;
use std::time::Instant;

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
}

impl EditorView {
    pub fn new(text: &str, file_label: impl Into<String>, cx: &mut Context<Self>) -> Self {
        let editor = Editor::new(text);
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
    ) -> Result<(), BufferError> {
        self.editor
            .set_selection(hane_editor::Selection::caret(hane_document::SourceOffset(
                offset,
            )))?;
        self.after_input(cx);
        Ok(())
    }

    #[cfg(debug_assertions)]
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
        let max_scroll = (self.heights.total_height() - self.viewport_height).max(0.0);
        self.scroll_y = self.scroll_y.clamp(0.0, max_scroll);
        let visible =
            self.heights
                .visible_range(self.scroll_y, self.viewport_height, self.theme.overscan);
        let top_space = self.heights.prefix_sum(visible.start);
        let bottom_space =
            (self.heights.total_height() - self.heights.prefix_sum(visible.end)).max(0.0);
        let revision = self.editor.document().revision().0;
        let bytes = self.editor.document().len_bytes().0;
        let p95 = self
            .metrics
            .painted_percentile(0.95)
            .map_or(0.0, |duration| duration.as_secs_f64() * 1000.0);
        let status = self
            .status
            .clone()
            .unwrap_or_else(|| format!("rev {revision} · {bytes} bytes · frame p95 {p95:.2} ms"));

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
        self.metrics.record_layout(layout_started.elapsed());
        root.child(
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
                        .children(visible.map(|line| line_element(&self.editor, line, self.theme)))
                        .child(div().h(px(bottom_space))),
                ),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hane_document::LineId;

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
}
