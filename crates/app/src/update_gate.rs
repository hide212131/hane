//! The application's startup gate (Issue #510, U4 increment 1).
//!
//! `main` calls [`enter`] before anything else: before the AI service, the
//! state stores, a window or any open request exists. While a pending update
//! transaction is being settled, or if it cannot be settled, the process never
//! reaches normal editing.

use hane_update::health::{COMMIT_WAIT, ChildEntry, detect_child};
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
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(err) => {
            // The placement cannot be resolved, so it cannot be ruled out that
            // an interrupted update is pending: do not start normal editing.
            eprintln!(
                "Hane could not finish or undo an interrupted update \
                 (the running executable could not be located: {err}). \
                 Install a signed Hane ZIP manually."
            );
            return Entry::Stop(EXIT_RECOVERY_REQUIRED);
        }
    };
    // The limited-health child of an update helper is the only launch that may
    // pass the gate without taking it, and only through the private capability
    // and pipes (never a flag or an environment variable). It starts nothing
    // else until the helper has made the commit durable.
    match detect_child(&exe, env!("CARGO_PKG_VERSION")) {
        ChildEntry::NotChild => {}
        ChildEntry::Refused(reason) => {
            eprintln!("Hane could not confirm the update it was started for ({reason}).");
            return Entry::Stop(EXIT_RECOVERY_REQUIRED);
        }
        ChildEntry::Child(session) => {
            // Read-only checks of the settings and the restart ticket belong
            // in this closure; they must not write anything.
            return match session.run(std::io::stdin(), std::io::stdout(), COMMIT_WAIT, || Ok(())) {
                Ok(guard) => Entry::Start(Some(guard)),
                Err(err) => {
                    eprintln!("Hane could not confirm the update it was started for ({err}).");
                    Entry::Stop(EXIT_RECOVERY_REQUIRED)
                }
            };
        }
    }
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
