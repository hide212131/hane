//! The application's startup gate (Issue #510, U4 increment 1).
//!
//! `main` calls [`enter`] before anything else: before the AI service, the
//! state stores, a window or any open request exists. While a pending update
//! transaction is being settled, or if it cannot be settled, the process never
//! reaches normal editing.

use std::path::PathBuf;

use hane_update::health::{COMMIT_WAIT, ChildEntry, detect_child};
use hane_update::startup::{GateOutcome, HelperRecoveryRunner, RuntimeGuard, startup_gate_for_exe};
use hane_update::trust::TrustPolicy;

/// Exit status of a launch that must not start the editor (EX_TEMPFAIL / EX_SOFTWARE).
const EXIT_UPDATE_IN_PROGRESS: i32 = 75;
const EXIT_RECOVERY_REQUIRED: i32 = 70;

pub enum Entry {
    /// Start normally. Hold the guard until the process ends: it is the
    /// shared runtime lease that tells a helper this process is still alive.
    /// `state_root` is set only for a committed update's child: the root the
    /// updating application used and the child verified read-only, which the
    /// normal start must use instead of resolving one from its own (empty)
    /// environment.
    Start {
        guard: Option<RuntimeGuard>,
        state_root: Option<PathBuf>,
    },
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
            // The read-only inspection of the state root, the settings store
            // and the restart ticket runs before `Healthy`; it writes nothing.
            // The root is handed on only if the helper's `Commit` arrives.
            let state_root = session.state_root().to_path_buf();
            let check = session.state_check();
            return match session.run(std::io::stdin(), std::io::stdout(), COMMIT_WAIT, move || {
                check.verify()
            }) {
                Ok(guard) => Entry::Start {
                    guard: Some(guard),
                    state_root: Some(state_root),
                },
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
        GateOutcome::Proceed(guard) => Entry::Start {
            guard: Some(guard),
            state_root: None,
        },
        GateOutcome::Unmanaged => Entry::Start {
            guard: None,
            state_root: None,
        },
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
