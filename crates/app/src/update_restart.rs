//! Hand-off from the U3 restart check to the update helper (U4 increments 1-2).
//!
//! A clear [`RestartPreparation`] is only an observation. This function turns
//! it into a started helper that has answered `Prepared`; it does not stop the
//! AI service, does not authorize the helper's exit and does not quit. The
//! caller must keep the editor running until it holds the returned handle, do
//! its own shutdown (AI stop, draft drain) and only then call
//! `PreparedRestart::authorize_exit` and quit. If anything here fails the
//! application stays open and untouched.
//!
//! The Ready candidate is copied into the helper's own transaction directory
//! here; the helper is never given a path and re-verifies the copy itself.
//!
//! The "restart and update" button is not enabled yet: the Windows file
//! replacement is connected, but the limited-health child and the durable
//! commit are not, so nothing may report an update as applied. macOS has no
//! apply adapter.

#![allow(dead_code, reason = "connected to the restart button by the next U4 increment")]

use std::path::Path;

use hane_ui::RestartPreparation;
use hane_update::helper::{PreparedRestart, prepare_restart};
use hane_update::platform::Placement;
use hane_update::trust::TrustPolicy;
use hane_update::windows_apply::CandidateSource;
use hane_update::{ErrorCode, UpdateError};

use crate::app_update::{ARCHIVE_FILE, CATALOG_FILE, SIGNATURE_FILE};

/// `candidate_dir` is the Ready candidate directory, which is named after
/// `candidate_id`; any other directory is refused. `state_root` is the running
/// application's own `FileStateStore::root()`; the new version uses exactly
/// that root, never a default it resolves itself.
pub fn start_helper(
    preparation: &RestartPreparation,
    placement: &Placement,
    policy: &TrustPolicy,
    candidate_id: &str,
    candidate_dir: &Path,
    state_root: &Path,
) -> Result<PreparedRestart, UpdateError> {
    match preparation {
        RestartPreparation::Clear(_) => {
            if candidate_dir.file_name().and_then(|name| name.to_str()) != Some(candidate_id) {
                return Err(UpdateError::new(
                    ErrorCode::IdentityMismatch,
                    "the candidate directory is not the requested candidate",
                ));
            }
            prepare_restart(
                placement,
                policy,
                candidate_id,
                &CandidateSource {
                    catalog: &candidate_dir.join(CATALOG_FILE),
                    signature: &candidate_dir.join(SIGNATURE_FILE),
                    archive: &candidate_dir.join(ARCHIVE_FILE),
                },
                state_root,
            )
        }
        RestartPreparation::Blocked(_) => Err(UpdateError::new(
            ErrorCode::NotApplicable,
            "the restart check found something that must be finished first",
        )),
    }
}
