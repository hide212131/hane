//! Hand-off from the U3 restart check to the update helper (U4 increment 1).
//!
//! A clear [`RestartPreparation`] is only an observation. This function turns
//! it into a started helper that has answered `Prepared`; it does not stop the
//! AI service, does not authorize the helper's exit and does not quit. The
//! caller must keep the editor running until it holds the returned handle, do
//! its own shutdown (AI stop, draft drain) and only then call
//! `PreparedRestart::authorize_exit` and quit. If anything here fails the
//! application stays open and untouched.
//!
//! The "restart and update" button is not enabled yet: no apply adapter is
//! connected, so nothing may report an update as applied.

#![allow(dead_code, reason = "connected to the restart button by the next U4 increment")]

use hane_ui::RestartPreparation;
use hane_update::helper::{PreparedRestart, prepare_restart};
use hane_update::platform::Placement;
use hane_update::trust::TrustPolicy;
use hane_update::{ErrorCode, UpdateError};

pub fn start_helper(
    preparation: &RestartPreparation,
    placement: &Placement,
    policy: &TrustPolicy,
    candidate_id: &str,
) -> Result<PreparedRestart, UpdateError> {
    match preparation {
        RestartPreparation::Clear(_) => prepare_restart(placement, policy, candidate_id),
        RestartPreparation::Blocked(_) => Err(UpdateError::new(
            ErrorCode::NotApplicable,
            "the restart check found something that must be finished first",
        )),
    }
}
