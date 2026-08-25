use gpui::{
    App, AppContext, Application, Bounds, IntoElement, Render, Styled, WindowBounds, WindowOptions,
    div, px, size,
};

struct BaselineView {
    reported: bool,
}

impl Render for BaselineView {
    fn render(&mut self, _: &mut gpui::Window, _: &mut gpui::Context<Self>) -> impl IntoElement {
        if !self.reported {
            self.reported = true;
            eprintln!(
                "hane_gpui_baseline rss_bytes={}",
                hane_benchmark::process_memory_bytes().unwrap_or(0)
            );
        }
        div().size_full()
    }
}

fn main() {
    Application::new().run(|cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(960.), px(760.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |_, cx| cx.new(|_| BaselineView { reported: false }),
        )
        .expect("open GPUI baseline window");
        cx.activate(true);
    });
}
