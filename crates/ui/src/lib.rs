//! GPUI adapter for the editor core. Only visible lines plus bounded overscan are rendered.

// GPUI's ownership, pixel-coordinate, callback, and palette APIs require patterns
// that pedantic normally discourages; keeping this boundary explicit avoids leaking
// those exceptions into the editor core.
#![allow(
    clippy::pedantic,
    reason = "the GPUI integration boundary intentionally follows GPUI callback and pixel-coordinate conventions"
)]

mod actions;
mod capture;
mod input;
#[cfg(any(feature = "instrument", feature = "timing-probe"))]
mod instrument;
mod line;
mod ranges;
mod shape;

mod theme;
mod view;

pub use actions::register_key_bindings;
#[cfg(feature = "instrument")]
pub use instrument::InstrumentationConfig;
pub use view::EditorView;
