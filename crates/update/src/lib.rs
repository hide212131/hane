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
//! U4 increment 1 adds process-lifetime leases, supported-placement checks,
//! the verified helper copy, the private helper protocol and the startup gate
//! (`platform`, `protocol`, `helper`, `startup`). Increment 2 connects the
//! Windows owned-file apply path to the helper (`windows_apply`): the helper
//! re-verifies a transaction-local candidate and replaces only the files of the
//! signed inventories, journal first. Without the limited-health child and the
//! durable commit it restores the old files again instead of reporting success.
//!
//! Deliberately **not** here: UI, GPUI, document or AI credential access, real
//! HTTP transport, bundle swapping, the limited-health child, durable commit,
//! and any production signing key. [`trust::TrustPolicy::production`] carries
//! no key material until a maintainer embeds one, so no candidate can become
//! applicable from a production build today. OS-specific locking and crash
//! durability are not claimed to be verified by this crate.

// Everything is safe Rust except the two read-only volume queries in
// `platform::{windows,macos}`, which opt in locally with a stated reason.
#![deny(unsafe_code)]

pub mod apply;
pub mod archive;
pub mod catalog;
pub mod clock;
pub mod download;
pub mod error;
pub mod fsutil;
pub mod health;
pub mod helper;
pub mod identity;
pub mod journal;
pub mod limits;
pub mod model;
pub mod paths;
pub mod platform;
pub mod protocol;
pub mod release;
pub mod startup;
pub mod trust;
mod util;
pub mod version;
pub mod windows_apply;

#[cfg(any(test, feature = "test-support"))]
pub mod testing;

#[cfg(test)]
mod tests;

pub use error::{ErrorCode, Result, UpdateError};
