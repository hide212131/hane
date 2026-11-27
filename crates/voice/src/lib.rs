//! Hane's side of the `voice/1` connection to the whisparrot engine.
//!
//! Hane finds the installed whisparrot ([`discovery`]), starts it as a child
//! with the connection secrets handed over an inherited anonymous pipe, accepts
//! its one authenticated connection on a dedicated pipe ([`link`]), and turns
//! the engine's events into the editor's voice events ([`wire`]). Nothing here
//! edits a document: the UI applies [`link::Inbound`] messages in its own turn.

pub mod discovery;
pub mod engine;
pub mod link;
pub mod transport;
#[cfg(unix)]
pub mod unix;
#[cfg(windows)]
mod windows;
pub mod wire;

pub use discovery::{Unavailable, discover, registration_path};
pub use engine::{EngineLocation, VoiceControl, VoiceEngine};
pub use link::{Failure, Inbound, Timeouts};
pub use wire::Context;
