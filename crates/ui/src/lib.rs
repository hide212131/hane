//! GPUI adapter for the editor core. Only visible lines plus bounded overscan are rendered.

mod actions;
mod capture;
#[cfg(feature = "instrument")]
mod instrument;
mod input;
mod line;
mod storage;
mod theme;
mod view;

pub use actions::register_key_bindings;
#[cfg(feature = "instrument")]
pub use instrument::InstrumentationConfig;
pub use view::EditorView;
