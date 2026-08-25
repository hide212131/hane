//! GPUI adapter for the editor core. Only visible lines plus bounded overscan are rendered.

mod actions;
mod capture;
mod input;
mod line;
mod phase0_metrics;
mod storage;
mod theme;
mod view;

pub use actions::register_key_bindings;
pub use view::EditorView;
