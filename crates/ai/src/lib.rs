//! Embedded Codex App Server runtime and stdio JSON-RPC transport.
//!
//! This crate is intentionally GPUI- and Tokio-free: it is plain
//! thread/channel Rust so it can be exercised and tested without pulling in
//! the UI toolchain. See `docs/adr/0032-embedded-codex-app-server-ai-foundation.md`
//! for the design this implements.

mod owner_lock;
mod protocol;
mod rpc;
mod runtime;

pub use owner_lock::{OwnerLock, OwnerLockGuard};
pub use protocol::{ErrorObject, IncomingMessage, ParseError, RequestId};
pub use rpc::{RejectAllServerRequests, RpcError, RpcEvent, ServerRequestHandler};
pub use runtime::{
    AiRuntime, RuntimeConfig, RuntimeError, RuntimeEvent, RuntimeEventKind, RuntimeState, RuntimeStatus,
};
