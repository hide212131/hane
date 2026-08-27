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
            let measurements = view.editor_mut().mark_frame_painted();
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
            view.record_frame_instrumentation(&measurements, interval, layout);
        });
    }
}
