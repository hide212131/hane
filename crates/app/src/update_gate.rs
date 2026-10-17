//! The application's startup gate (Issue #510, U4 increment 1).
//!
//! `main` calls [`enter`] before anything else: before the AI service, the
//! state stores, a window or any open request exists. While a pending update
//! transaction is being settled, or if it cannot be settled, the process never
//! reaches normal editing.

use hane_update::startup::{GateOutcome, HelperRecoveryRunner, RuntimeGuard, startup_gate_for_exe};
use hane_update::trust::TrustPolicy;

/// Exit status of a launch that must not start the editor (EX_TEMPFAIL / EX_SOFTWARE).
const EXIT_UPDATE_IN_PROGRESS: i32 = 75;
const EXIT_RECOVERY_REQUIRED: i32 = 70;

pub enum Entry {
    /// Start normally. Hold the guard until the process ends: it is the
    /// shared runtime lease that tells a helper this process is still alive.
    Start(Option<RuntimeGuard>),
    /// Do not start; exit the process with this status.
    Stop(i32),
}

pub fn enter() -> Entry {
    let Ok(exe) = std::env::current_exe() else {
        return Entry::Start(None);
    };
    let runner = HelperRecoveryRunner {
        policy: TrustPolicy::production(),
    };
    match startup_gate_for_exe(&exe, &runner) {
        GateOutcome::Proceed(guard) => Entry::Start(Some(guard)),
        GateOutcome::Unmanaged => Entry::Start(None),
        GateOutcome::UpdateInProgress => {
            eprintln!("Hane is being updated. Please start it again in a moment.");
            Entry::Stop(EXIT_UPDATE_IN_PROGRESS)
        }
        GateOutcome::RecoveryRequired(reason) => {
            eprintln!(
                "Hane could not finish or undo an interrupted update ({reason}). \
                 Install a signed Hane ZIP manually."
            );
            Entry::Stop(EXIT_RECOVERY_REQUIRED)
        }
    }
}
