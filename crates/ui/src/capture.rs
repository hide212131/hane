use crate::phase0_metrics::log_summary;
use crate::view::EditorView;
use gpui::{
    App, Bounds, Element, ElementId, ElementInputHandler, Entity, GlobalElementId, IntoElement,
    LayoutId, Pixels, Style, Window, px, relative,
};
use std::time::Instant;

pub(crate) struct InputCapture {
    pub input: Entity<EditorView>,
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
            let painted_at = Instant::now();
            let measurements = view.editor.mark_frame_painted();
            let model_latencies = measurements
                .iter()
                .map(|measurement| measurement.keystroke_to_model());
            let frame_latencies = measurements
                .iter()
                .filter_map(|measurement| measurement.keystroke_to_frame());
            let interval = view
                .metrics
                .record_paint(painted_at, model_latencies, frame_latencies);
            let layout = view.metrics.latest_layout();
            if view.ready_armed && !view.ready_reported {
                view.ready_reported = true;
                let startup = view.process_started.elapsed();
                let rss = process_rss_bytes();
                eprintln!(
                    "hane_ready startup_time_ms={:.3} file_open_time_ms={:.3} rss_bytes={}",
                    startup.as_secs_f64() * 1_000.0,
                    view.file_open_time.as_secs_f64() * 1_000.0,
                    rss.unwrap_or(0),
                );
                if let Some(output) = &mut view.metrics_output {
                    if let Err(error) = output.memory("memory_load", view.load_rss_bytes) {
                        eprintln!("could not write load memory metrics: {error}");
                    }
                    if let Err(error) = output.ready(startup, view.file_open_time, rss) {
                        eprintln!("could not write ready metrics: {error}");
                    }
                }
            }
            if let Some(output) = &mut view.metrics_output {
                if let Err(error) = output.paint(interval, layout) {
                    eprintln!("could not write paint metrics: {error}");
                }
                for measurement in &measurements {
                    if let Err(error) = output.input(measurement) {
                        eprintln!("could not write input metrics: {error}");
                    }
                }
            }
            if !measurements.is_empty() {
                log_summary(&view.metrics);
            }
        });
    }
}

fn process_rss_bytes() -> Option<u64> {
    hane_metrics::process_memory_bytes()
}
