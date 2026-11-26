//! Paper / wall texture in the editor view (Issue #788, phase P1).
//!
//! The view keeps at most one generated image and at most one generation in
//! flight. Both belong to a [`TextureKey`] (theme, kind, strength, scale
//! factor); the image is generated on the background executor only when the
//! key changes, and the previous image is handed back to the window at that
//! moment. Until the new image is ready the body is drawn plain. Scrolling,
//! resizing, typing and zooming never touch this state: they only change where
//! the finished tiles are placed.

use super::*;
use crate::paper_texture::{PaperTexture, TextureKey, generate_tile};
use crate::theme::DARK_THEME;
use gpui::RenderImage;
use hane_session::PaperTextureStrength;

/// The cached image, and the generation that will replace it.
pub(super) struct PaperTextureState {
    ready: Option<(TextureKey, Arc<RenderImage>)>,
    /// The key being generated and the task doing it. Dropping the task
    /// cancels the generation.
    pending: Option<(TextureKey, Task<()>)>,
    /// How many generations were started. Read by tests.
    generations: usize,
    /// How many images were handed back to the window. Read by tests.
    released: usize,
}

impl PaperTextureState {
    pub(super) fn new() -> Self {
        Self {
            ready: None,
            pending: None,
            generations: 0,
            released: 0,
        }
    }

    /// Takes the cached image out so the caller can return it to the window.
    pub(super) fn take_image(&mut self) -> Option<Arc<RenderImage>> {
        self.pending = None;
        self.ready.take().map(|(_, image)| image)
    }
}

impl EditorView {
    pub(crate) fn paper_texture_enabled(&self) -> bool {
        self.settings.paper_texture
    }

    pub(crate) fn paper_texture_strength(&self) -> PaperTextureStrength {
        self.settings.paper_texture_strength
    }

    /// Turns the texture on or off. Only the display depends on it.
    pub(crate) fn set_paper_texture(&mut self, on: bool, cx: &mut Context<Self>) {
        if self.settings.paper_texture == on {
            return;
        }
        self.settings.paper_texture = on;
        // A failed write keeps the change for this run, like the theme.
        self.store_settings();
        cx.notify();
    }

    pub(crate) fn set_paper_texture_strength(
        &mut self,
        strength: PaperTextureStrength,
        cx: &mut Context<Self>,
    ) {
        if self.settings.paper_texture_strength == strength {
            return;
        }
        self.settings.paper_texture_strength = strength;
        self.store_settings();
        cx.notify();
    }

    /// The layer to draw behind the body this frame. `None` while the texture
    /// is off (the element is then not part of the tree at all) and while the
    /// image for the current key is still being generated.
    pub(super) fn paper_texture_layer(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<PaperTexture> {
        let scale_factor = window.scale_factor();
        self.paper_texture_layer_at(scale_factor, window, cx)
    }

    /// [`Self::paper_texture_layer`] for a given scale factor.
    fn paper_texture_layer_at(
        &mut self,
        scale_factor: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<PaperTexture> {
        if !self.settings.paper_texture {
            self.release_paper_texture(window, cx);
            return None;
        }
        let key = TextureKey::new(
            self.theme == DARK_THEME,
            self.settings.paper_texture_strength,
            scale_factor,
        );
        if self
            .paper_texture
            .ready
            .as_ref()
            .is_some_and(|(ready, _)| *ready != key)
        {
            self.release_paper_texture(window, cx);
        }
        if let Some((_, image)) = &self.paper_texture.ready {
            return Some(PaperTexture::new(Arc::clone(image), self.scroll_y));
        }
        if self
            .paper_texture
            .pending
            .as_ref()
            .is_none_or(|(pending, _)| *pending != key)
        {
            self.start_paper_texture(key, self.theme.editor_background, cx);
        }
        None
    }

    /// Hands the cached image back to the window and cancels a running
    /// generation.
    fn release_paper_texture(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(image) = self.paper_texture.take_image() {
            cx.drop_image(image, Some(&mut *window));
            self.paper_texture.released += 1;
        }
    }

    fn start_paper_texture(&mut self, key: TextureKey, background: u32, cx: &mut Context<Self>) {
        self.paper_texture.generations += 1;
        let task = cx.spawn(async move |view, cx| {
            let image = cx
                .background_executor()
                .spawn(async move { generate_tile(&key, background).into_render_image() })
                .await;
            let _ = view.update(cx, |view, cx| view.finish_paper_texture(key, image, cx));
        });
        self.paper_texture.pending = Some((key, task));
    }

    /// Takes a finished image. One whose key was replaced in the meantime is
    /// dropped without ever having reached the window.
    fn finish_paper_texture(
        &mut self,
        key: TextureKey,
        image: Arc<RenderImage>,
        cx: &mut Context<Self>,
    ) {
        if self
            .paper_texture
            .pending
            .as_ref()
            .is_none_or(|(pending, _)| *pending != key)
        {
            return;
        }
        self.paper_texture.pending = None;
        self.paper_texture.ready = Some((key, image));
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestView = gpui::Entity<EditorView>;
    type TestCx = gpui::VisualTestContext;

    fn settle(cx: &mut TestCx) {
        cx.run_until_parked();
        cx.executor()
            .advance_clock(std::time::Duration::from_millis(100));
        cx.run_until_parked();
    }

    fn open_view(
        cx: &mut gpui::TestAppContext,
        stores: StateStores,
    ) -> (TestView, &mut TestCx) {
        let (view, cx) = cx.add_window_view(move |_, cx| {
            EditorView::from_sessions(
                SessionSet::with_untitled("# Title\n\nbody\n", "Untitled"),
                Arc::new(OsFileService),
                stores,
                cx,
            )
        });
        cx.simulate_resize(gpui::size(px(800.0), px(600.0)));
        settle(cx);
        (view, cx)
    }

    /// Draws one more frame and lets a finished generation land.
    fn frame(view: &TestView, cx: &mut TestCx) {
        view.update(cx, |_, cx| cx.notify());
        cx.run_until_parked();
    }

    fn counters(view: &TestView, cx: &mut TestCx) -> (usize, usize) {
        view.read_with(cx, |view, _| {
            (view.paper_texture.generations, view.paper_texture.released)
        })
    }

    fn image_id(view: &TestView, cx: &mut TestCx) -> Option<usize> {
        view.read_with(cx, |view, _| {
            view.paper_texture
                .ready
                .as_ref()
                .map(|(_, image)| image.id.0)
        })
    }

    fn layer_present(view: &TestView, cx: &mut TestCx) -> bool {
        cx.update(|window, app| {
            view.update(app, |view, cx| view.paper_texture_layer(window, cx).is_some())
        })
    }

    #[gpui::test]
    fn texture_is_off_by_default_and_no_layer_enters_the_tree(cx: &mut gpui::TestAppContext) {
        let (view, cx) = open_view(cx, StateStores::memory());
        view.read_with(cx, |view, _| {
            assert!(!view.paper_texture_enabled());
            assert_eq!(
                view.paper_texture_strength(),
                PaperTextureStrength::Standard
            );
        });
        for _ in 0..3 {
            frame(&view, cx);
            assert!(!layer_present(&view, cx));
        }
        assert_eq!(counters(&view, cx), (0, 0));
        assert_eq!(image_id(&view, cx), None);
    }

    #[gpui::test]
    fn the_settings_are_stored(cx: &mut gpui::TestAppContext) {
        let stores = StateStores::memory();
        let (view, cx) = open_view(cx, stores.clone());
        view.update(cx, |view, cx| {
            view.set_paper_texture(true, cx);
            view.set_paper_texture_strength(PaperTextureStrength::Strong, cx);
        });
        let stored = stores.settings().load();
        assert!(stored.paper_texture);
        assert_eq!(stored.paper_texture_strength, PaperTextureStrength::Strong);
        view.update(cx, |view, cx| view.set_paper_texture(false, cx));
        assert!(!stores.settings().load().paper_texture);
    }

    #[gpui::test]
    fn the_image_is_generated_once_and_scrolling_does_not_regenerate(
        cx: &mut gpui::TestAppContext,
    ) {
        let (view, cx) = open_view(cx, StateStores::memory());
        view.update(cx, |view, cx| view.set_paper_texture(true, cx));
        frame(&view, cx);
        assert_eq!(counters(&view, cx), (1, 0));
        let first = image_id(&view, cx);
        assert!(first.is_some(), "the generation finished");
        assert!(layer_present(&view, cx));
        for scroll in [10.0_f32, 123.5, 400.0, 0.0] {
            view.update(cx, |view, cx| {
                view.scroll_y = scroll;
                cx.notify();
            });
            frame(&view, cx);
            assert!(layer_present(&view, cx));
        }
        cx.simulate_resize(gpui::size(px(900.0), px(700.0)));
        frame(&view, cx);
        assert_eq!(counters(&view, cx), (1, 0));
        assert_eq!(image_id(&view, cx), first);
    }

    #[gpui::test]
    fn a_strength_change_regenerates_exactly_once(cx: &mut gpui::TestAppContext) {
        let (view, cx) = open_view(cx, StateStores::memory());
        view.update(cx, |view, cx| view.set_paper_texture(true, cx));
        frame(&view, cx);
        let first = image_id(&view, cx);
        view.update(cx, |view, cx| {
            view.set_paper_texture_strength(PaperTextureStrength::Strong, cx);
        });
        frame(&view, cx);
        assert_eq!(counters(&view, cx), (2, 1));
        let second = image_id(&view, cx);
        assert!(second.is_some());
        assert_ne!(first, second);
        for _ in 0..3 {
            frame(&view, cx);
        }
        assert_eq!(counters(&view, cx), (2, 1));
        assert_eq!(image_id(&view, cx), second);
    }

    #[gpui::test]
    fn a_theme_change_regenerates_exactly_once(cx: &mut gpui::TestAppContext) {
        let (view, cx) = open_view(cx, StateStores::memory());
        view.update(cx, |view, cx| view.set_paper_texture(true, cx));
        frame(&view, cx);
        let first = image_id(&view, cx);
        cx.update(|window, app| {
            view.update(app, |view, cx| {
                view.set_theme(ThemePreference::Dark, window, cx);
            });
        });
        frame(&view, cx);
        assert_eq!(counters(&view, cx), (2, 1));
        assert_ne!(first, image_id(&view, cx));
        for _ in 0..3 {
            frame(&view, cx);
        }
        assert_eq!(counters(&view, cx), (2, 1));
    }

    #[gpui::test]
    fn a_scale_factor_change_regenerates_exactly_once(cx: &mut gpui::TestAppContext) {
        let (view, cx) = open_view(cx, StateStores::memory());
        view.update(cx, |view, cx| view.set_paper_texture(true, cx));
        frame(&view, cx);
        let first = image_id(&view, cx);
        // The test window's scale factor is fixed, so the changed factor is
        // handed to the layer directly, the way `window.scale_factor()` would.
        let layer_at = |view: &TestView, cx: &mut TestCx, scale: f32| {
            cx.update(|window, app| {
                view.update(app, |view, cx| {
                    view.paper_texture_layer_at(scale, window, cx).is_some()
                })
            })
        };
        // Nothing is parked in between, so no frame at the window's own scale
        // factor interferes: the old image goes back to the window and one
        // generation starts.
        assert!(!layer_at(&view, cx, 1.5));
        assert_eq!(counters(&view, cx), (2, 1));
        assert_eq!(image_id(&view, cx), None);
        assert_ne!(first, None);
        // The same factor again, and one equal after rounding to hundredths,
        // keep waiting for that one generation.
        assert!(!layer_at(&view, cx, 1.5));
        assert!(!layer_at(&view, cx, 1.501));
        assert_eq!(counters(&view, cx), (2, 1));
    }

    #[gpui::test]
    fn turning_the_texture_off_returns_the_image(cx: &mut gpui::TestAppContext) {
        let (view, cx) = open_view(cx, StateStores::memory());
        view.update(cx, |view, cx| view.set_paper_texture(true, cx));
        frame(&view, cx);
        assert!(image_id(&view, cx).is_some());
        view.update(cx, |view, cx| view.set_paper_texture(false, cx));
        frame(&view, cx);
        assert_eq!(image_id(&view, cx), None);
        assert_eq!(counters(&view, cx), (1, 1));
        assert!(!layer_present(&view, cx));
    }

    #[gpui::test]
    fn the_texture_changes_neither_layout_nor_document_state(cx: &mut gpui::TestAppContext) {
        let snapshot = |view: &TestView, cx: &mut TestCx| {
            frame(view, cx);
            view.read_with(cx, |view, _| {
                (
                    view.editor().document().revision(),
                    view.editor().document().full_text(),
                    view.sessions.active().is_dirty(),
                    view.editor().can_undo(),
                    view.editor().can_redo(),
                    view.scrollable_content_height().to_bits(),
                )
            })
        };
        let (view, cx) = open_view(cx, StateStores::memory());
        let plain = snapshot(&view, cx);
        view.update(cx, |view, cx| view.set_paper_texture(true, cx));
        let textured = snapshot(&view, cx);
        assert_eq!(plain, textured);
    }
}
