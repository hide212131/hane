//! GPUI adapter for the editor core. Only visible lines plus bounded overscan are rendered.

// Some GPUI assertion macro expansions report their source span in `core`; retain
// the layout module's exact-comparison policy at the crate boundary for that case.
#![allow(
    clippy::float_cmp,
    reason = "GPUI geometry assertions require exact comparisons and macro expansion cannot be scoped to the call site"
)]

mod actions;
mod capture;
pub mod context_menu;
mod icons;
mod input;
mod input_mode;
#[cfg(any(feature = "instrument", feature = "timing-probe"))]
mod instrument;
mod line;
mod ranges;
mod shape;

mod theme;
mod view;

pub use actions::register_key_bindings;
pub use icons::WorkFolderIcons;
#[cfg(feature = "instrument")]
pub use instrument::InstrumentationConfig;
pub use view::EditorView;

/// Tracks the settings component setup once per GPUI application.
pub(crate) struct ComponentsInitialized;
impl gpui::Global for ComponentsInitialized {}

/// Initializes GPUI components used by the file tabs and settings screen.
pub fn init_components(cx: &mut gpui::App) {
    if cx.has_global::<ComponentsInitialized>() {
        return;
    }
    gpui_component::init(cx);
    cx.set_global(ComponentsInitialized);
}
