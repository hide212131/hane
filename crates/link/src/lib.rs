//! The local link between Hane and a voice-input application.
//!
//! A framed JSON protocol over a per-user, access-controlled channel (a Windows
//! named pipe in the product). This crate holds everything that does not need
//! the editor or the UI: framing, authentication, version negotiation, the
//! operation ledger, range handles and range tracking, and context slicing.
//! Reads and edits are executed by the editor side through [`host::Host`].

pub mod auth;
pub mod context;
pub mod descriptor;
pub mod frame;
pub mod frontmatter;
pub mod handles;
pub mod host;
pub mod ledger;
pub mod message;
pub mod platform;
mod private_fs;
pub mod runtime;
pub mod selection;
pub mod server;
pub mod service;
pub mod testing;
pub mod track;
pub mod transport;
#[cfg(windows)]
pub mod windows;

pub use auth::Credentials;
pub use host::{Host, HostCall, QueueHost, SubmitError, TickClock, Ticket};
pub use message::{Effect, ErrorCode, HostRequest, LinkError, LinkResult};
pub use service::RunningService;
