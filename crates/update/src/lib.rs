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
//! signed inventories, journal first. Increment 3 adds the limited-health child
//! and the durable commit (`health`). Increment 4 connects the macOS bundle
//! exchange (`macos_apply`): the pinned Team ID's Developer ID signature, the
//! bundle identity and a same-volume `RENAME_SWAP` of the live bundle and the
//! staged candidate, journaled intent first, with the same health and commit
//! boundary and a rollback by the same exchange.
//!
//! Deliberately **not** here: UI, GPUI, document or AI credential access, real
//! HTTP transport, the restart UI, and any production signing key or Team ID.
//! [`trust::TrustPolicy::production`] carries no key material until a
//! maintainer embeds one, so no candidate can become applicable from a
//! production build today. OS-specific locking, crash durability, the real
//! `renamex_np` / `codesign` / `spctl` behavior and Gatekeeper are not claimed
//! to be verified by this crate.

// Everything is safe Rust except the volume queries in `platform::windows` and
// `platform::macos` (and the macOS `renamex_np` exchange), which opt in locally
// with a stated reason.
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
pub mod macos_apply;
pub mod model;
pub mod paths;
pub mod platform;
mod pinned;
pub mod protocol;
pub mod release;
pub mod root_guard;
pub mod startup;
pub mod state_root;
pub mod trust;
mod util;
pub mod version;
pub mod windows_apply;

#[cfg(any(test, feature = "test-support"))]
pub mod testing;

#[cfg(test)]
mod tests;

pub use error::{ErrorCode, Result, UpdateError};
