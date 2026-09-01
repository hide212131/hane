//! GPUI adapter for the editor core. Only visible lines plus bounded overscan are rendered.

// Some GPUI assertion macro expansions report their source span in `core`; retain
// the layout module's exact-comparison policy at the crate boundary for that case.
#![allow(
    clippy::float_cmp,
    reason = "GPUI geometry assertions require exact comparisons and macro expansion cannot be scoped to the call site"
)]

mod actions;
mod capture;
mod icons;
mod input;
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
