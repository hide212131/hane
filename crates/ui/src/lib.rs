//! GPUI adapter for the editor core. Only visible lines plus bounded overscan are rendered.

mod actions;
mod capture;
mod input;
mod line;
#[cfg(feature = "instrument")]
mod phase0_metrics;
mod storage;
mod theme;
mod view;

pub use actions::register_key_bindings;
#[cfg(feature = "instrument")]
pub use phase0_metrics::InstrumentationConfig;
pub use view::EditorView;
