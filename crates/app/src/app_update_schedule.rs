//! Automatic update-check schedule (Issue #649).
//!
//! The schedule is plain data driven by a monotonic clock that callers pass
//! in, so the 30 second start, the hourly interval, the shared failure backoff
//! and a long sleep are testable without waiting. It is never persisted and
//! makes no request itself: [`AutoSchedule::auto_origin`] only says whether an
//! automatic check may start, and the service decides whether it can.
//!
//! The wait thread ([`run_timer`]) sleeps on one condition variable until the
//! next deadline, a state change or a stop. It performs no network work; at
//! the deadline it asks the service to start a check through the service's own
//! runner.

use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, Weak};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::app_update::Inner;

/// How long after the normal window is shown the first check may start.
pub(crate) const STARTUP_DELAY: Duration = Duration::from_secs(30);
/// Time from a completed check to the next automatic one.
pub(crate) const CHECK_INTERVAL: Duration = Duration::from_secs(60 * 60);

/// Wait after the `failures`-th consecutive failed check: 1, 2, then 4 hours.
pub(crate) fn backoff_after(failures: u32) -> Duration {
    let hours = match failures {
        0 | 1 => 1,
        2 => 2,
        _ => 4,
    };
    Duration::from_secs(hours * 60 * 60)
}

/// Who started a check. Fixed when the check starts.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CheckOrigin {
    /// The first automatic check after start-up.
    Startup,
    /// Any later automatic check.
    Periodic,
    /// A button press.
    Manual,
}

/// How a finished check counts for the schedule.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CheckHealth {
    Success,
    Failure,
    /// Stopped by the user; neither a success nor a failure.
    Cancelled,
}

/// Monotonic time as the offset from an arbitrary fixed point. Separate from
/// the wall clock that validates catalog expiry, so a changed system time
/// never causes back-to-back checks.
pub(crate) trait Monotonic: Send + Sync {
    fn now(&self) -> Duration;
}

pub(crate) struct SystemMonotonic {
    base: Instant,
}

impl SystemMonotonic {
    pub(crate) fn new() -> Self {
        Self { base: Instant::now() }
    }
}

impl Monotonic for SystemMonotonic {
    fn now(&self) -> Duration {
        self.base.elapsed()
    }
}

/// Next deadline, backoff floor and pending flags of the automatic check.
#[derive(Debug, Default)]
pub(crate) struct AutoSchedule {
    armed: bool,
    paused: bool,
    /// When the next automatic check is due. `None` while a check runs.
    due: Option<Duration>,
    /// The first automatic check has not started yet.
    startup_pending: bool,
    failures: u32,
    /// No check, manual or automatic, starts before this after a failure.
    retry_not_before: Option<Duration>,
}

impl AutoSchedule {
    /// Starts the schedule: the first check is due after [`STARTUP_DELAY`].
    /// `false` when it was already armed.
    pub(crate) fn arm(&mut self, now: Duration) -> bool {
        if self.armed {
            return false;
        }
        self.armed = true;
        self.startup_pending = true;
        self.due = Some(now + STARTUP_DELAY);
        true
    }

    pub(crate) fn disarm(&mut self) {
        self.armed = false;
        self.due = None;
    }

    pub(crate) fn set_paused(&mut self, paused: bool) {
        self.paused = paused;
    }

    /// The origin an automatic check would have if it started now. A deadline
    /// that passed while the check could not run (or while the machine slept)
    /// yields one check, never several.
    pub(crate) fn auto_origin(&self, now: Duration) -> Option<CheckOrigin> {
        if !self.armed || self.paused {
            return None;
        }
        match self.due {
            Some(due) if now >= due => Some(if self.startup_pending {
                CheckOrigin::Startup
            } else {
                CheckOrigin::Periodic
            }),
            _ => None,
        }
    }

    /// A manual check is not allowed to shorten a failure backoff.
    pub(crate) fn manual_allowed(&self, now: Duration) -> bool {
        self.retry_not_before.is_none_or(|until| now >= until)
    }

    /// How long a manual check is still refused; `None` when it is allowed.
    pub(crate) fn manual_wait(&self, now: Duration) -> Option<Duration> {
        self.retry_not_before
            .and_then(|until| until.checked_sub(now))
            .filter(|wait| !wait.is_zero())
    }

    /// A check started. No further automatic check is due until it finishes.
    pub(crate) fn started(&mut self, origin: CheckOrigin) {
        self.due = None;
        if origin != CheckOrigin::Periodic {
            self.startup_pending = false;
        }
    }

    /// A check finished. Success resets the failure count and waits one
    /// interval; a failure waits the shared backoff, for automatic and manual
    /// checks alike.
    pub(crate) fn finished(&mut self, now: Duration, health: CheckHealth) {
        match health {
            CheckHealth::Success => {
                self.failures = 0;
                self.retry_not_before = None;
            }
            CheckHealth::Failure => {
                self.failures = self.failures.saturating_add(1);
                self.retry_not_before = Some(now + backoff_after(self.failures));
            }
            CheckHealth::Cancelled => {
                // A stale, already elapsed backoff must not make the next
                // automatic check due right after the cancel.
                if self.retry_not_before.is_some_and(|until| now >= until) {
                    self.retry_not_before = None;
                }
            }
        }
        if self.armed {
            self.due = Some(match (health, self.retry_not_before) {
                (CheckHealth::Cancelled, Some(until)) => until.max(now + CHECK_INTERVAL),
                (_, Some(until)) => until,
                (_, None) => now + CHECK_INTERVAL,
            });
        }
    }

    /// How long the wait thread may sleep. `None` means "until something
    /// changes": nothing is scheduled, or the deadline passed and only a state
    /// change can let the check start.
    pub(crate) fn next_wait(&self, now: Duration) -> Option<Duration> {
        if !self.armed || self.paused {
            return None;
        }
        let wait = self.due?.saturating_sub(now);
        (!wait.is_zero()).then_some(wait)
    }
}

#[derive(Default)]
struct SignalState {
    stopped: bool,
    generation: u64,
}

/// Wakes the wait thread on a state change or a stop.
pub(crate) struct Signal {
    state: Mutex<SignalState>,
    changed: Condvar,
}

impl Signal {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(SignalState::default()),
            changed: Condvar::new(),
        })
    }

    fn lock(&self) -> MutexGuard<'_, SignalState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Something the schedule depends on changed: re-evaluate.
    pub(crate) fn poke(&self) {
        self.lock().generation += 1;
        self.changed.notify_all();
    }

    pub(crate) fn stop(&self) {
        let mut state = self.lock();
        state.stopped = true;
        state.generation += 1;
        drop(state);
        self.changed.notify_all();
    }

    /// The generation to wait on, or `None` once stopped.
    fn generation(&self) -> Option<u64> {
        let state = self.lock();
        (!state.stopped).then_some(state.generation)
    }

    /// Blocks until `timeout` elapses (forever for `None`), the generation
    /// moves past `seen` or the signal stops. `false` once stopped.
    fn wait(&self, seen: u64, timeout: Option<Duration>) -> bool {
        let deadline = timeout.map(|timeout| Instant::now() + timeout);
        let mut state = self.lock();
        loop {
            if state.stopped {
                return false;
            }
            if state.generation != seen {
                return true;
            }
            state = match deadline {
                None => self
                    .changed
                    .wait(state)
                    .unwrap_or_else(PoisonError::into_inner),
                Some(deadline) => {
                    let left = deadline.saturating_duration_since(Instant::now());
                    if left.is_zero() {
                        return true;
                    }
                    self.changed
                        .wait_timeout(state, left)
                        .unwrap_or_else(PoisonError::into_inner)
                        .0
                }
            };
        }
    }
}

/// Owns the wait thread. Dropping it stops and joins the thread, so the timer
/// ends with the service that owns the guard.
pub(crate) struct AutoCheckGuard {
    signal: Arc<Signal>,
    thread: Option<JoinHandle<()>>,
}

impl AutoCheckGuard {
    pub(crate) fn spawn(inner: &Arc<Inner>, signal: Arc<Signal>) -> std::io::Result<Self> {
        let weak = Arc::downgrade(inner);
        let thread_signal = Arc::clone(&signal);
        let thread = std::thread::Builder::new()
            .name("hane-app-update-timer".to_owned())
            .spawn(move || run_timer(&weak, &thread_signal))?;
        Ok(Self {
            signal,
            thread: Some(thread),
        })
    }
}

impl Drop for AutoCheckGuard {
    fn drop(&mut self) {
        self.signal.stop();
        if let Some(thread) = self.thread.take()
            && thread.thread().id() != std::thread::current().id()
        {
            let _ = thread.join();
        }
    }
}

/// The wait loop. It holds the service only while it evaluates the schedule,
/// never while it sleeps, so it cannot keep a replaced service alive.
fn run_timer(inner: &Weak<Inner>, signal: &Signal) {
    loop {
        let Some(seen) = signal.generation() else {
            return;
        };
        let Some(strong) = inner.upgrade() else {
            return;
        };
        let wait = strong.auto_step();
        drop(strong);
        if !signal.wait(seen, wait) {
            return;
        }
    }
}

#[cfg(test)]
pub(crate) mod testing {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// A monotonic clock the test moves by hand.
    #[derive(Default)]
    pub(crate) struct FakeMonotonic {
        millis: AtomicU64,
    }

    impl FakeMonotonic {
        pub(crate) fn advance(&self, by: Duration) {
            self.millis
                .fetch_add(u64::try_from(by.as_millis()).unwrap_or(u64::MAX), Ordering::SeqCst);
        }
    }

    impl Monotonic for FakeMonotonic {
        fn now(&self) -> Duration {
            Duration::from_millis(self.millis.load(Ordering::SeqCst))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECOND: Duration = Duration::from_secs(1);
    const HOUR: Duration = Duration::from_secs(3600);

    #[test]
    fn nothing_is_due_before_arming_and_the_first_check_waits_thirty_seconds() {
        let mut schedule = AutoSchedule::default();
        assert_eq!(schedule.auto_origin(HOUR * 5), None);
        assert!(schedule.arm(Duration::from_secs(10)));
        assert!(!schedule.arm(Duration::from_secs(11)), "arming twice adds no timer");
        assert_eq!(schedule.auto_origin(Duration::from_secs(39)), None);
        assert_eq!(
            schedule.next_wait(Duration::from_secs(10)),
            Some(STARTUP_DELAY)
        );
        assert_eq!(
            schedule.auto_origin(Duration::from_secs(40)),
            Some(CheckOrigin::Startup)
        );
    }

    #[test]
    fn a_success_waits_one_hour_from_the_completion_and_later_checks_are_periodic() {
        let mut schedule = AutoSchedule::default();
        schedule.arm(Duration::ZERO);
        schedule.started(CheckOrigin::Startup);
        assert_eq!(schedule.auto_origin(HOUR * 9), None, "no check while one runs");
        let done = Duration::from_secs(45);
        schedule.finished(done, CheckHealth::Success);
        assert_eq!(schedule.auto_origin(done + HOUR - SECOND), None);
        assert_eq!(schedule.auto_origin(done + HOUR), Some(CheckOrigin::Periodic));
    }

    #[test]
    fn a_long_sleep_yields_one_check_not_the_missed_ones() {
        let mut schedule = AutoSchedule::default();
        schedule.arm(Duration::ZERO);
        schedule.started(CheckOrigin::Startup);
        schedule.finished(SECOND, CheckHealth::Success);
        let woke = HOUR * 30;
        assert_eq!(schedule.auto_origin(woke), Some(CheckOrigin::Periodic));
        schedule.started(CheckOrigin::Periodic);
        assert_eq!(schedule.auto_origin(woke), None);
        schedule.finished(woke, CheckHealth::Success);
        assert_eq!(schedule.auto_origin(woke + HOUR - SECOND), None);
    }

    #[test]
    fn failures_back_off_one_two_then_four_hours_and_success_resets() {
        let mut schedule = AutoSchedule::default();
        schedule.arm(Duration::ZERO);
        schedule.started(CheckOrigin::Startup);
        schedule.finished(Duration::ZERO, CheckHealth::Success);
        let mut now = HOUR;
        for hours in [1, 2, 4, 4] {
            schedule.started(CheckOrigin::Periodic);
            schedule.finished(now, CheckHealth::Failure);
            let wait = HOUR * hours;
            assert_eq!(schedule.auto_origin(now + wait - SECOND), None);
            assert_eq!(schedule.auto_origin(now + wait), Some(CheckOrigin::Periodic));
            now += wait;
        }
        schedule.started(CheckOrigin::Periodic);
        schedule.finished(now, CheckHealth::Success);
        assert!(schedule.manual_allowed(now));
        schedule.started(CheckOrigin::Periodic);
        schedule.finished(now, CheckHealth::Failure);
        assert_eq!(
            schedule.auto_origin(now + HOUR),
            Some(CheckOrigin::Periodic),
            "a success resets the count to the first one-hour step"
        );
    }

    #[test]
    fn a_manual_retry_cannot_shorten_the_backoff_but_a_manual_failure_counts() {
        let mut schedule = AutoSchedule::default();
        schedule.arm(Duration::ZERO);
        let now = Duration::from_secs(40);
        schedule.started(CheckOrigin::Startup);
        schedule.finished(now, CheckHealth::Failure);
        assert!(!schedule.manual_allowed(now + HOUR - SECOND));
        assert!(schedule.manual_allowed(now + HOUR));
        // A manual failure after the wait is the second one: two hours.
        schedule.started(CheckOrigin::Manual);
        schedule.finished(now + HOUR, CheckHealth::Failure);
        assert!(!schedule.manual_allowed(now + HOUR * 3 - SECOND));
        assert!(schedule.manual_allowed(now + HOUR * 3));
    }

    #[test]
    fn the_manual_wait_matches_the_manual_allowed_boundary() {
        let mut schedule = AutoSchedule::default();
        schedule.arm(Duration::ZERO);
        let now = Duration::from_secs(40);
        assert_eq!(schedule.manual_wait(now), None);
        schedule.started(CheckOrigin::Startup);
        schedule.finished(now, CheckHealth::Failure);
        assert_eq!(schedule.manual_wait(now), Some(HOUR));
        assert_eq!(schedule.manual_wait(now + HOUR - SECOND), Some(SECOND));
        assert!(!schedule.manual_allowed(now + HOUR - SECOND));
        assert_eq!(schedule.manual_wait(now + HOUR), None);
        assert!(schedule.manual_allowed(now + HOUR));
    }

    #[test]
    fn a_manual_success_resets_the_backoff_and_moves_the_next_check_an_hour_out() {
        let mut schedule = AutoSchedule::default();
        schedule.arm(Duration::ZERO);
        schedule.started(CheckOrigin::Startup);
        schedule.finished(SECOND, CheckHealth::Failure);
        let later = SECOND + HOUR;
        schedule.started(CheckOrigin::Manual);
        schedule.finished(later, CheckHealth::Success);
        assert!(schedule.manual_allowed(later));
        assert_eq!(schedule.auto_origin(later + HOUR - SECOND), None);
        assert_eq!(schedule.auto_origin(later + HOUR), Some(CheckOrigin::Periodic));
    }

    #[test]
    fn a_manual_check_before_the_first_automatic_one_consumes_the_startup_origin() {
        let mut schedule = AutoSchedule::default();
        schedule.arm(Duration::ZERO);
        schedule.started(CheckOrigin::Manual);
        schedule.finished(SECOND, CheckHealth::Success);
        assert_eq!(schedule.auto_origin(HOUR + SECOND), Some(CheckOrigin::Periodic));
    }

    #[test]
    fn a_cancelled_check_neither_counts_as_failure_nor_leaves_the_schedule_empty() {
        let mut schedule = AutoSchedule::default();
        schedule.arm(Duration::ZERO);
        schedule.started(CheckOrigin::Startup);
        schedule.finished(SECOND, CheckHealth::Cancelled);
        assert!(schedule.manual_allowed(SECOND));
        assert_eq!(schedule.auto_origin(SECOND + HOUR), Some(CheckOrigin::Periodic));
    }

    #[test]
    fn a_cancel_after_an_elapsed_backoff_waits_a_full_interval() {
        let mut schedule = AutoSchedule::default();
        schedule.arm(Duration::ZERO);
        schedule.started(CheckOrigin::Startup);
        schedule.finished(SECOND, CheckHealth::Failure);
        let later = SECOND + HOUR * 3;
        schedule.started(CheckOrigin::Manual);
        schedule.finished(later, CheckHealth::Cancelled);
        assert_eq!(schedule.auto_origin(later), None);
        assert_eq!(schedule.auto_origin(later + HOUR - SECOND), None);
        assert_eq!(schedule.auto_origin(later + HOUR), Some(CheckOrigin::Periodic));
    }

    #[test]
    fn pausing_stops_automatic_checks_and_resuming_restores_them() {
        let mut schedule = AutoSchedule::default();
        schedule.arm(Duration::ZERO);
        schedule.set_paused(true);
        assert_eq!(schedule.auto_origin(HOUR), None);
        assert_eq!(schedule.next_wait(Duration::ZERO), None);
        schedule.set_paused(false);
        assert_eq!(schedule.auto_origin(HOUR), Some(CheckOrigin::Startup));
    }

    #[test]
    fn a_passed_deadline_waits_for_a_state_change_instead_of_spinning() {
        let mut schedule = AutoSchedule::default();
        schedule.arm(Duration::ZERO);
        assert_eq!(schedule.next_wait(HOUR), None);
    }

    #[test]
    fn the_signal_wakes_on_poke_times_out_and_stops() {
        let signal = Signal::new();
        let seen = signal.generation().unwrap();
        assert!(signal.wait(seen, Some(Duration::from_millis(20))), "timeout re-evaluates");
        signal.poke();
        assert!(signal.wait(seen, None), "a poke after reading is not lost");
        signal.stop();
        assert!(!signal.wait(seen, None));
        assert_eq!(signal.generation(), None);
    }
}
