//! Shared core for application self-update (Issue #510, stage U0).
//!
//! This crate turns network-delivered bytes into a verified, staged candidate
//! and defines the types a later OS adapter needs to apply it safely:
//!
//! - typed catalog model, SemVer / target / release-asset selection
//! - schema 1 catalog verification over the raw signed bytes
//! - bounded download / staging behind transport, clock and filesystem seams
//! - whole-archive ZIP inspection before anything is extracted
//! - install identity, signed inventory, `ApplyPlan`, `UpdateJournal` and the
//!   journal-versus-disk recovery decision
//!
//! Deliberately **not** here: UI, GPUI, document or AI credential access, real
//! HTTP transport, process locks, file replacement, bundle swapping, and any
//! production signing key. [`trust::TrustPolicy::production`] carries no key
//! material until a maintainer embeds one, so no candidate can become
//! applicable from a production build today. OS-specific locking and crash
//! durability are not claimed to be verified by this crate.

#![forbid(unsafe_code)]

pub mod archive;
pub mod catalog;
pub mod clock;
pub mod download;
pub mod error;
pub mod fsutil;
pub mod identity;
pub mod journal;
pub mod limits;
pub mod model;
pub mod paths;
pub mod release;
pub mod trust;
mod util;
pub mod version;

#[cfg(any(test, feature = "test-support"))]
pub mod testing;

#[cfg(test)]
mod tests;

pub use error::{ErrorCode, Result, UpdateError};
