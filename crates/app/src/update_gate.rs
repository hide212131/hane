//! The application's startup gate (Issue #510, U4 increment 1).
//!
//! `main` calls [`enter`] before anything else: before the AI service, the
//! state stores, a window or any open request exists. While a pending update
//! transaction is being settled, or if it cannot be settled, the process never
//! reaches normal editing.

use std::path::PathBuf;

use hane_update::health::{COMMIT_WAIT, ChildEntry, VerifiedChild, detect_child};
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
    /// The limited-health child of an update helper. Only the minimal update
    /// UI may start; `Healthy` follows its first drawn frame and normal startup
    /// follows the helper's durable `Commit`.
    Limited(Box<VerifiedChild>),
    /// Do not start; exit the process with this status.
    Stop(i32),
}

/// How long the limited child waits for the helper's `Commit` after `Healthy`.
pub const LIMITED_COMMIT_WAIT: std::time::Duration = COMMIT_WAIT;

/// How long a launch carrying an explicit file argument keeps waiting for an
/// update in progress, so the request is delivered instead of dropped.
const EXPLICIT_OPEN_WAIT: std::time::Duration = std::time::Duration::from_secs(60);
const EXPLICIT_OPEN_RETRY_INTERVAL: std::time::Duration = std::time::Duration::from_millis(250);

/// Takes the startup gate. With an explicit open request, `UpdateInProgress`
/// is retried (each attempt takes the gate again) until `wait` has elapsed in
/// total sleep time; every other outcome is returned at once.
fn gate_with_open_retry(
    mut take_gate: impl FnMut() -> GateOutcome,
    has_explicit_open: bool,
    wait: std::time::Duration,
    mut sleep: impl FnMut(std::time::Duration),
) -> GateOutcome {
    let mut waited = std::time::Duration::ZERO;
    loop {
        let outcome = take_gate();
        if !has_explicit_open
            || !matches!(outcome, GateOutcome::UpdateInProgress)
            || waited >= wait
        {
            return outcome;
        }
        sleep(EXPLICIT_OPEN_RETRY_INTERVAL);
        waited += EXPLICIT_OPEN_RETRY_INTERVAL;
    }
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
            // `Healthy` is not sent here: the limited UI must first draw its
            // first frame (see `limited_start`).
            let check = session.state_check();
            return match session.verify(move || check.verify()) {
                Ok(verified) => Entry::Limited(Box::new(verified)),
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
    // An explicit file argument must not be lost to an update in progress: keep
    // taking the gate (never bypassing it) and let this process deliver the path
    // through the ordinary open path once the gate admits it.
    let has_explicit_open = std::env::args_os().nth(1).is_some();
    let outcome = gate_with_open_retry(
        || startup_gate_for_exe(&exe, &runner),
        has_explicit_open,
        EXPLICIT_OPEN_WAIT,
        std::thread::sleep,
    );
    match outcome {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn scripted(in_progress: usize) -> impl FnMut() -> GateOutcome {
        let mut remaining = in_progress;
        move || {
            if remaining > 0 {
                remaining -= 1;
                GateOutcome::UpdateInProgress
            } else {
                GateOutcome::Unmanaged
            }
        }
    }

    #[test]
    fn an_explicit_open_waits_out_the_update_and_is_admitted_once() {
        let mut admitted = 0;
        let mut inner = scripted(3);
        let mut sleeps = 0;
        let outcome = gate_with_open_retry(
            || {
                let outcome = inner();
                if matches!(outcome, GateOutcome::Unmanaged) {
                    admitted += 1;
                }
                outcome
            },
            true,
            Duration::from_secs(60),
            |_| sleeps += 1,
        );
        assert!(matches!(outcome, GateOutcome::Unmanaged));
        assert_eq!(admitted, 1, "the launch (and its path) is admitted exactly once");
        assert_eq!(sleeps, 3);
    }

    #[test]
    fn a_launch_without_an_explicit_open_still_stops_while_updating() {
        let mut sleeps = 0;
        let outcome = gate_with_open_retry(scripted(3), false, Duration::from_secs(60), |_| sleeps += 1);
        assert!(matches!(outcome, GateOutcome::UpdateInProgress));
        assert_eq!(sleeps, 0);
    }

    #[test]
    fn an_explicit_open_gives_up_after_the_wait_without_bypassing_the_gate() {
        let mut sleeps = 0;
        let outcome = gate_with_open_retry(
            || GateOutcome::UpdateInProgress,
            true,
            Duration::from_secs(1),
            |_| sleeps += 1,
        );
        assert!(matches!(outcome, GateOutcome::UpdateInProgress));
        assert_eq!(sleeps, 4);
    }
}
