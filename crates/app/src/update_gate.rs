//! The application's startup gate (Issue #510, U4 increment 1).
//!
//! `main` calls [`enter`] before anything else: before the AI service, the
//! state stores, a window or any open request exists. While a pending update
//! transaction is being settled, or if it cannot be settled, the process never
//! reaches normal editing.

use std::path::PathBuf;

use hane_update::health::{COMMIT_WAIT, ChildEntry, VerifiedChild, detect_child};
use hane_update::startup::{
    GateOutcome, HelperRecoveryRunner, RuntimeGuard, STARTUP_WAIT, startup_gate_for_exe_within,
};
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

/// Takes the startup gate. With an open request, `UpdateInProgress` is retried
/// (each attempt takes the gate again). `wait` is the total real-time budget,
/// including the time spent inside the gate calls; `None` means no budget: the
/// gate is retaken until it reaches a terminal outcome (a Finder event is
/// delivered only once by LaunchServices, so the process must outlive any
/// update). Every other outcome is returned at once. `elapsed` reports the time
/// since the start, and each attempt is told how long it may wait for the
/// startup lease: never more than the remaining budget.
fn gate_with_open_retry(
    mut take_gate: impl FnMut(std::time::Duration) -> GateOutcome,
    has_explicit_open: bool,
    wait: Option<std::time::Duration>,
    mut sleep: impl FnMut(std::time::Duration),
    mut elapsed: impl FnMut() -> std::time::Duration,
) -> GateOutcome {
    loop {
        let attempt_wait = match wait {
            Some(wait) if has_explicit_open => wait.saturating_sub(elapsed()).min(STARTUP_WAIT),
            _ => STARTUP_WAIT,
        };
        let outcome = take_gate(attempt_wait);
        if !has_explicit_open || !matches!(outcome, GateOutcome::UpdateInProgress) {
            return outcome;
        }
        let pause = match wait {
            Some(wait) => {
                let remaining = wait.saturating_sub(elapsed());
                if remaining.is_zero() {
                    return outcome;
                }
                EXPLICIT_OPEN_RETRY_INTERVAL.min(remaining)
            }
            None => EXPLICIT_OPEN_RETRY_INTERVAL,
        };
        sleep(pause);
    }
}

/// The retry budget for a launch that may carry an open request. A macOS launch
/// may receive a Finder document that LaunchServices delivers only once, so it
/// has no time limit and ends only on a terminal gate outcome; a launch with
/// just a command-line path keeps the bounded wait on Linux. A Windows launch
/// with a command-line path is likewise unbounded: the one-shot request is held
/// until the gate reaches a terminal outcome instead of expiring after 60s.
fn open_request_wait(has_cli_path: bool) -> Option<std::time::Duration> {
    open_request_wait_for(
        has_cli_path,
        cfg!(target_os = "macos"),
        cfg!(target_os = "windows"),
    )
}

fn open_request_wait_for(
    has_cli_path: bool,
    is_macos: bool,
    is_windows: bool,
) -> Option<std::time::Duration> {
    if is_macos || (is_windows && has_cli_path) || !has_cli_path {
        None
    } else {
        Some(EXPLICIT_OPEN_WAIT)
    }
}

/// Whether this launch may carry a document to open. On macOS, LaunchServices
/// delivers Finder documents through `on_open_urls` rather than argv, and only
/// once the application runs, which is after this gate. Such a launch cannot be
/// told apart from an ordinary one, so every macOS launch waits out an update in
/// progress: the pending Finder event stays with the OS until the gate admits
/// normal startup and is then queued by the callback registered before `run`
/// and opened once through the normal open flow. The gate itself is never
/// bypassed, and `RecoveryRequired` still stops the launch.
fn may_carry_open_request(has_cli_path: bool) -> bool {
    has_cli_path || cfg!(target_os = "macos")
}

/// Whether the first argument is an actual file path to open. The context menu
/// management flags carry no open request, so they keep the bounded gate.
fn is_cli_open_path(first_arg: Option<&std::ffi::OsStr>) -> bool {
    match first_arg {
        None => false,
        Some(arg) => arg != "--register-context-menu" && arg != "--unregister-context-menu",
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
    let has_cli_path = is_cli_open_path(std::env::args_os().nth(1).as_deref());
    let has_explicit_open = may_carry_open_request(has_cli_path);
    let started = std::time::Instant::now();
    let outcome = gate_with_open_retry(
        |wait| startup_gate_for_exe_within(&exe, &runner, wait),
        has_explicit_open,
        open_request_wait(has_cli_path),
        std::thread::sleep,
        || started.elapsed(),
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

    fn scripted(in_progress: usize) -> impl FnMut(Duration) -> GateOutcome {
        let mut remaining = in_progress;
        move |_| {
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
            |wait| {
                let outcome = inner(wait);
                if matches!(outcome, GateOutcome::Unmanaged) {
                    admitted += 1;
                }
                outcome
            },
            true,
            Some(Duration::from_secs(60)),
            |_| sleeps += 1,
            || Duration::ZERO,
        );
        assert!(matches!(outcome, GateOutcome::Unmanaged));
        assert_eq!(admitted, 1, "the launch (and its path) is admitted exactly once");
        assert_eq!(sleeps, 3);
    }

    #[test]
    fn a_launch_that_may_receive_a_finder_open_waits_for_the_update() {
        assert!(may_carry_open_request(true));
        assert_eq!(may_carry_open_request(false), cfg!(target_os = "macos"));
    }

    #[test]
    fn a_finder_open_before_update_completion_is_opened_once_after_admission() {
        let queue = hane_ui::SharedExternalOpenQueue::new();
        // The event is queued while the update is still in progress.
        queue.push(PathBuf::from("/notes/a.md"));
        let mut opened = Vec::new();
        let mut inner = scripted(2);
        let outcome = gate_with_open_retry(
            |wait| {
                let outcome = inner(wait);
                if matches!(outcome, GateOutcome::UpdateInProgress) {
                    // Nothing opens before the gate admits normal editing.
                    assert!(opened.is_empty());
                }
                outcome
            },
            true,
            Some(Duration::from_secs(60)),
            |_| {},
            || Duration::ZERO,
        );
        assert!(matches!(outcome, GateOutcome::Unmanaged));
        for _ in 0..2 {
            queue.drain_into(|path| {
                opened.push(path.to_path_buf());
                hane_ui::ExternalOpenOutcome::Accepted
            });
        }
        assert_eq!(opened, vec![PathBuf::from("/notes/a.md")]);
        assert!(queue.is_empty());
    }

    #[test]
    fn a_launch_without_an_explicit_open_still_stops_while_updating() {
        let mut sleeps = 0;
        let outcome = gate_with_open_retry(
            scripted(3),
            false,
            Some(Duration::from_secs(60)),
            |_| sleeps += 1,
            || Duration::ZERO,
        );
        assert!(matches!(outcome, GateOutcome::UpdateInProgress));
        assert_eq!(sleeps, 0);
    }

    #[test]
    fn an_explicit_open_gives_up_after_the_wait_without_bypassing_the_gate() {
        let clock = std::cell::Cell::new(Duration::ZERO);
        let mut sleeps = 0;
        let outcome = gate_with_open_retry(
            |_| GateOutcome::UpdateInProgress,
            true,
            Some(Duration::from_secs(1)),
            |d| {
                sleeps += 1;
                clock.set(clock.get() + d);
            },
            || clock.get(),
        );
        assert!(matches!(outcome, GateOutcome::UpdateInProgress));
        assert_eq!(sleeps, 4);
    }

    #[test]
    fn a_launch_without_a_time_limit_outlives_the_old_budget_and_is_admitted_once() {
        let clock = std::cell::Cell::new(Duration::ZERO);
        let queue = hane_ui::SharedExternalOpenQueue::new();
        let mut admitted = 0;
        let mut opened = Vec::new();
        let outcome = gate_with_open_retry(
            |wait| {
                clock.set(clock.get() + wait);
                // The update outlasts both the 60s budget and many attempts.
                if clock.get() <= Duration::from_secs(600) {
                    assert!(opened.is_empty(), "nothing opens before admission");
                    GateOutcome::UpdateInProgress
                } else {
                    admitted += 1;
                    GateOutcome::Unmanaged
                }
            },
            true,
            None,
            |d| clock.set(clock.get() + d),
            || clock.get(),
        );
        assert!(matches!(outcome, GateOutcome::Unmanaged));
        assert_eq!(admitted, 1);
        assert!(clock.get() > Duration::from_secs(600));
        // The Finder event arrives only after admission and opens exactly once.
        queue.push(PathBuf::from("/notes/a.md"));
        for _ in 0..2 {
            queue.drain_into(|path| {
                opened.push(path.to_path_buf());
                hane_ui::ExternalOpenOutcome::Accepted
            });
        }
        assert_eq!(opened, vec![PathBuf::from("/notes/a.md")]);
    }

    #[test]
    fn a_launch_without_a_time_limit_still_stops_on_recovery_required() {
        let mut calls = 0;
        let outcome = gate_with_open_retry(
            |_| {
                calls += 1;
                if calls < 3 {
                    GateOutcome::UpdateInProgress
                } else {
                    GateOutcome::RecoveryRequired("broken".into())
                }
            },
            true,
            None,
            |_| {},
            || Duration::ZERO,
        );
        assert!(matches!(outcome, GateOutcome::RecoveryRequired(_)));
        assert_eq!(calls, 3);
    }

    #[test]
    fn only_macos_and_windows_cli_launches_are_unbounded() {
        assert_eq!(open_request_wait(false), None);
        if cfg!(any(target_os = "macos", target_os = "windows")) {
            assert_eq!(open_request_wait(true), None);
        } else {
            assert_eq!(open_request_wait(true), Some(EXPLICIT_OPEN_WAIT));
        }
        // Linux CLI stays bounded; macOS and Windows CLI are unbounded.
        assert_eq!(
            open_request_wait_for(true, false, false),
            Some(EXPLICIT_OPEN_WAIT)
        );
        assert_eq!(open_request_wait_for(true, true, false), None);
        assert_eq!(open_request_wait_for(true, false, true), None);
        assert_eq!(open_request_wait_for(false, true, false), None);
    }

    #[test]
    fn a_windows_cli_open_outlives_the_old_60s_limit_and_is_handed_over_once() {
        let wait = open_request_wait_for(true, false, true);
        assert_eq!(wait, None);
        let clock = std::cell::Cell::new(Duration::ZERO);
        let mut admitted = 0;
        let mut opened = Vec::new();
        let outcome = gate_with_open_retry(
            |attempt| {
                clock.set(clock.get() + attempt);
                if clock.get() <= Duration::from_secs(120) {
                    GateOutcome::UpdateInProgress
                } else {
                    admitted += 1;
                    GateOutcome::Unmanaged
                }
            },
            true,
            wait,
            |d| clock.set(clock.get() + d),
            || clock.get(),
        );
        assert!(matches!(outcome, GateOutcome::Unmanaged));
        assert_eq!(admitted, 1);
        assert!(clock.get() > Duration::from_secs(60));
        // Only after admission is the path passed to the normal open flow, once.
        if matches!(outcome, GateOutcome::Unmanaged) {
            opened.push(PathBuf::from("C:\\notes\\a.md"));
        }
        assert_eq!(opened, vec![PathBuf::from("C:\\notes\\a.md")]);
    }

    #[test]
    fn context_menu_flags_are_not_open_paths_but_ordinary_paths_are() {
        use std::ffi::OsStr;
        assert!(!is_cli_open_path(None));
        assert!(!is_cli_open_path(Some(OsStr::new("--register-context-menu"))));
        assert!(!is_cli_open_path(Some(OsStr::new("--unregister-context-menu"))));
        assert!(is_cli_open_path(Some(OsStr::new("C:\\notes\\a.md"))));
        assert!(is_cli_open_path(Some(OsStr::new("a.md"))));
        // A management flag keeps the bounded gate on Windows; a path waits
        // for the terminal outcome.
        let flag = is_cli_open_path(Some(OsStr::new("--register-context-menu")));
        let gate = gate_with_open_retry(
            |_| GateOutcome::UpdateInProgress,
            flag,
            open_request_wait_for(flag, false, true),
            |_| panic!("a management flag never retries"),
            || Duration::ZERO,
        );
        assert!(matches!(gate, GateOutcome::UpdateInProgress));
        assert_eq!(open_request_wait_for(true, false, true), None);
    }

    #[test]
    fn a_linux_cli_open_stays_bounded() {
        let wait = open_request_wait_for(true, false, false);
        assert_eq!(wait, Some(EXPLICIT_OPEN_WAIT));
        let clock = std::cell::Cell::new(Duration::ZERO);
        let outcome = gate_with_open_retry(
            |attempt| {
                clock.set(clock.get() + attempt);
                GateOutcome::UpdateInProgress
            },
            true,
            wait,
            |d| clock.set(clock.get() + d),
            || clock.get(),
        );
        assert!(matches!(outcome, GateOutcome::UpdateInProgress));
        assert!(clock.get() <= Duration::from_secs(60));
    }

    #[test]
    fn time_spent_inside_gate_calls_counts_toward_the_total_wait() {
        let clock = std::cell::Cell::new(Duration::ZERO);
        let mut calls = 0;
        let mut granted = Vec::new();
        let outcome = gate_with_open_retry(
            |wait| {
                calls += 1;
                granted.push(wait);
                // Each attempt consumes its whole grant, like a lease timeout.
                clock.set(clock.get() + wait);
                GateOutcome::UpdateInProgress
            },
            true,
            Some(Duration::from_secs(60)),
            |d| clock.set(clock.get() + d),
            || clock.get(),
        );
        assert!(matches!(outcome, GateOutcome::UpdateInProgress));
        assert!(
            clock.get() <= Duration::from_secs(60),
            "total wait {:?} exceeded the budget",
            clock.get()
        );
        assert!(granted.iter().all(|w| *w <= STARTUP_WAIT));
        assert!(calls <= 3, "{calls} gate calls cannot fit a 60s budget of 30s waits");
    }
}
