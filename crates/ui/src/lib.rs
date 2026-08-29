//! GPUI adapter for the editor core. Only visible lines plus bounded overscan are rendered.

mod actions;
mod capture;
mod input;
#[cfg(any(feature = "instrument", feature = "timing-probe"))]
mod instrument;
mod line;
mod shape;

mod theme;
mod view;

pub use actions::register_key_bindings;
#[cfg(feature = "instrument")]
pub use instrument::InstrumentationConfig;
pub use view::EditorView;
