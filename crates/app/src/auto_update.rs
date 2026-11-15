//! Timing and history of the automatic update check (Issue #723).
//!
//! Pure scheduling and one small persisted record; no network, no state
//! machine. The update service decides *what* an automatic check does
//! ([`crate::app_update`]); this module decides *when* and remembers the last
//! attempt and the "後で" suppression across restarts.
//!
//! The history is kept apart from `settings.conf` because it changes often:
//! `<state root>/updates/<InstallId>/auto-check.conf`. It is never used for the
//! signed sequence floor, the journal or the "updated" decision.

use std::fmt::Write as _;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use hane_update::fsutil::{durable_write, read_limited};

/// How long after start the first automatic check may run.
pub(crate) const FIRST_CHECK_DELAY_SECS: i64 = 30;
/// Interval between automatic checks, and how long "後で" suppresses a version.
pub(crate) const CHECK_INTERVAL_SECS: i64 = 6 * 60 * 60;
pub(crate) const SNOOZE_SECS: i64 = CHECK_INTERVAL_SECS;
/// One wait never lasts longer, so a clock that jumped (sleep, resume) is
/// looked at again soon and an overdue cycle runs once, not once per missed
/// interval.
pub(crate) const MAX_WAIT: Duration = Duration::from_secs(60);

const HISTORY_FILE: &str = "auto-check.conf";
const HISTORY_SCHEMA: &str = "1";
const HISTORY_LIMIT: u64 = 4096;

/// Seconds to wait before the first check of this run: at least 30 seconds, and
/// until six hours after the last recorded attempt. A saved time in the future
/// is an anomaly (the clock went back); it must not stop the checks, so it is
/// treated as "no usable record".
pub(crate) fn initial_delay_secs(last_attempt: Option<i64>, now: i64) -> i64 {
    match last_attempt {
        Some(last) if last <= now => {
            let next = last.saturating_add(CHECK_INTERVAL_SECS);
            next.saturating_sub(now).max(FIRST_CHECK_DELAY_SECS)
        }
        _ => FIRST_CHECK_DELAY_SECS,
    }
}

/// The in-process deadline of the next automatic attempt, on the monotonic
/// clock. It is armed once per run and again after every accepted attempt,
/// whatever its outcome, so a failure waits for the next interval too.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Schedule {
    due_ms: u64,
}

impl Schedule {
    pub(crate) fn new(now_ms: u64, delay_secs: i64) -> Self {
        Self {
            due_ms: now_ms.saturating_add(secs_to_ms(delay_secs)),
        }
    }

    pub(crate) fn is_due(&self, now_ms: u64) -> bool {
        now_ms >= self.due_ms
    }

    /// How long to sleep before looking again.
    pub(crate) fn wait(&self, now_ms: u64) -> Duration {
        Duration::from_millis(self.due_ms.saturating_sub(now_ms)).min(MAX_WAIT)
    }

    /// An attempt was accepted: the next one is one interval away.
    pub(crate) fn attempted(&mut self, now_ms: u64) {
        *self = Self::new(now_ms, CHECK_INTERVAL_SECS);
    }
}

fn secs_to_ms(secs: i64) -> u64 {
    u64::try_from(secs).unwrap_or(0).saturating_mul(1000)
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct Record {
    last_attempt_unix: Option<i64>,
    /// `<target>/<version>` the "後で" applies to. The candidate id is not
    /// part of it: a re-issued catalog of the same version must not lift the
    /// suppression.
    prompt_key: Option<String>,
    prompt_not_before_unix: Option<i64>,
    /// Set when the file could not be trusted: no prompt until this time. It is
    /// part of the record so a rewrite of a healthy file keeps it across
    /// restarts.
    hold_until_unix: Option<i64>,
}

fn parse_record(bytes: &[u8]) -> Option<Record> {
    let text = std::str::from_utf8(bytes).ok()?;
    let mut schema_ok = false;
    let mut record = Record::default();
    for line in text.lines().filter(|line| !line.is_empty()) {
        let (key, value) = line.split_once('=')?;
        match key {
            "schema" => schema_ok = value == HISTORY_SCHEMA,
            "last_attempt_unix" => record.last_attempt_unix = Some(value.parse().ok()?),
            "prompt_key" => record.prompt_key = Some(value.to_owned()),
            "prompt_not_before_unix" => {
                record.prompt_not_before_unix = Some(value.parse().ok()?);
            }
            "hold_until_unix" => record.hold_until_unix = Some(value.parse().ok()?),
            _ => {}
        }
    }
    let consistent = record.prompt_key.is_some() == record.prompt_not_before_unix.is_some();
    (schema_ok && consistent).then_some(record)
}

fn render_record(record: &Record) -> String {
    let mut text = format!("schema={HISTORY_SCHEMA}\n");
    if let Some(last) = record.last_attempt_unix {
        let _ = writeln!(text, "last_attempt_unix={last}");
    }
    if let (Some(key), Some(until)) = (&record.prompt_key, record.prompt_not_before_unix) {
        let _ = writeln!(text, "prompt_key={key}");
        let _ = writeln!(text, "prompt_not_before_unix={until}");
    }
    if let Some(hold) = record.hold_until_unix {
        let _ = writeln!(text, "hold_until_unix={hold}");
    }
    text
}

struct HistoryState {
    /// `record.hold_until_unix` is set when the file existed but could not be
    /// trusted: no prompt until then, so a broken history cannot make every
    /// start prompt again. It is written with the next save.
    record: Record,
}

/// The persisted last attempt and prompt suppression of one install.
pub(crate) struct History {
    path: PathBuf,
    state: Mutex<HistoryState>,
}

impl History {
    /// Reads the history below `updates_root`. A missing file is a normal first
    /// run; an unreadable or malformed one starts empty but holds prompts back
    /// for one interval from `now`.
    pub(crate) fn open(updates_root: &Path, now: i64) -> Self {
        let path = updates_root.join(HISTORY_FILE);
        let held = || Record {
            hold_until_unix: Some(now.saturating_add(SNOOZE_SECS)),
            ..Record::default()
        };
        let record = match read_limited(&path, HISTORY_LIMIT) {
            Ok(None) => Record::default(),
            Ok(Some(bytes)) => parse_record(&bytes).unwrap_or_else(held),
            Err(_) => held(),
        };
        Self {
            path,
            state: Mutex::new(HistoryState { record }),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HistoryState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(crate) fn last_attempt(&self) -> Option<i64> {
        self.lock().record.last_attempt_unix
    }

    /// Remembers an accepted attempt. The in-memory value is kept even when
    /// the write fails, so this run still waits; the error is for diagnostics.
    pub(crate) fn record_attempt(&self, now: i64) -> io::Result<()> {
        let mut state = self.lock();
        state.record.last_attempt_unix = Some(now);
        self.persist(&state.record)
    }

    /// Suppresses the prompt for `key` until `until`.
    pub(crate) fn snooze(&self, key: &str, until: i64) -> io::Result<()> {
        let mut state = self.lock();
        state.record.prompt_key = Some(key.to_owned());
        state.record.prompt_not_before_unix = Some(until);
        self.persist(&state.record)
    }

    /// True while a prompt for `key` must not be shown at `now`.
    pub(crate) fn snoozed(&self, key: &str, now: i64) -> bool {
        let state = self.lock();
        // Like a snooze time, a hold further away than one interval is an
        // anomaly and does not hide the prompt.
        if state
            .record
            .hold_until_unix
            .is_some_and(|until| until > now && until - now <= SNOOZE_SECS)
        {
            return true;
        }
        match (&state.record.prompt_key, state.record.prompt_not_before_unix) {
            (Some(saved), Some(until)) if saved == key => {
                // A time further away than one suppression can be is an
                // anomaly (the clock went back) and does not hide the prompt.
                until > now && until - now <= SNOOZE_SECS
            }
            _ => false,
        }
    }

    fn persist(&self, record: &Record) -> io::Result<()> {
        if let Some(parent) = self.path.parent() {
            crate::app_update::create_private_dir(parent)?;
        }
        durable_write(&self.path, render_record(record).as_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hane_update::clock::Clock;
    use hane_update::testing::{FixedClock, TempDir, test_now};

    const KEY: &str = "x86_64-pc-windows-msvc/0.27.0";

    fn schedule_after(clock: &FixedClock, last: Option<i64>) -> Schedule {
        Schedule::new(
            clock.monotonic_ms(),
            initial_delay_secs(last, clock.now_unix()),
        )
    }

    #[test]
    fn first_check_waits_thirty_seconds() {
        let clock = FixedClock::new(test_now());
        let schedule = schedule_after(&clock, None);
        assert!(!schedule.is_due(clock.monotonic_ms()));
        clock.advance(29_999);
        assert!(!schedule.is_due(clock.monotonic_ms()));
        assert_eq!(schedule.wait(clock.monotonic_ms()), Duration::from_millis(1));
        clock.advance(1);
        assert!(schedule.is_due(clock.monotonic_ms()));
    }

    #[test]
    fn recent_attempt_throttles_across_restart() {
        let clock = FixedClock::new(test_now());
        // The previous process tried one hour ago.
        let last = clock.now_unix() - 3600;
        let schedule = schedule_after(&clock, Some(last));
        clock.advance(31_000);
        assert!(!schedule.is_due(clock.monotonic_ms()));
        clock.advance((5 * 3600 - 31) * 1000 - 1);
        assert!(!schedule.is_due(clock.monotonic_ms()));
        clock.advance(1);
        assert!(schedule.is_due(clock.monotonic_ms()));

        // An old attempt still waits the 30 seconds.
        let old = schedule_after(&clock, Some(clock.now_unix() - 7 * 3600));
        assert!(!old.is_due(clock.monotonic_ms()));
    }

    #[test]
    fn failed_attempt_waits_until_next_interval() {
        let clock = FixedClock::new(test_now());
        let mut schedule = schedule_after(&clock, None);
        clock.advance(30_000);
        assert!(schedule.is_due(clock.monotonic_ms()));
        // The attempt is accepted whether it later succeeds or fails.
        schedule.attempted(clock.monotonic_ms());
        clock.advance(60_000);
        assert!(!schedule.is_due(clock.monotonic_ms()));
        clock.advance(CHECK_INTERVAL_SECS as u64 * 1000 - 60_000 - 1);
        assert!(!schedule.is_due(clock.monotonic_ms()));
        clock.advance(1);
        assert!(schedule.is_due(clock.monotonic_ms()));
    }

    #[test]
    fn resume_runs_one_overdue_cycle() {
        let clock = FixedClock::new(test_now());
        let mut schedule = schedule_after(&clock, None);
        // The machine slept for a day: one cycle is due, and after it runs the
        // next one is a full interval away rather than a backlog.
        clock.advance(24 * 3600 * 1000);
        assert!(schedule.is_due(clock.monotonic_ms()));
        schedule.attempted(clock.monotonic_ms());
        assert!(!schedule.is_due(clock.monotonic_ms()));
    }

    #[test]
    fn future_timestamp_does_not_disable_checks_forever() {
        let now = test_now();
        assert_eq!(initial_delay_secs(Some(now + 10 * 24 * 3600), now), FIRST_CHECK_DELAY_SECS);
        assert_eq!(initial_delay_secs(None, now), FIRST_CHECK_DELAY_SECS);
        assert_eq!(initial_delay_secs(Some(now), now), CHECK_INTERVAL_SECS);
    }

    #[test]
    fn waits_are_cut_to_one_minute() {
        let clock = FixedClock::new(test_now());
        let schedule = Schedule::new(clock.monotonic_ms(), CHECK_INTERVAL_SECS);
        assert_eq!(schedule.wait(clock.monotonic_ms()), MAX_WAIT);
    }

    #[test]
    fn last_attempt_is_persisted() {
        let dir = TempDir::new("auto-update-attempt");
        let history = History::open(dir.path(), test_now());
        assert_eq!(history.last_attempt(), None);
        history.record_attempt(test_now()).unwrap();
        let reopened = History::open(dir.path(), test_now());
        assert_eq!(reopened.last_attempt(), Some(test_now()));
    }

    #[test]
    fn later_survives_process_restart() {
        let dir = TempDir::new("auto-update-later");
        let now = test_now();
        let history = History::open(dir.path(), now);
        assert!(!history.snoozed(KEY, now));
        history.snooze(KEY, now + SNOOZE_SECS).unwrap();
        assert!(history.snoozed(KEY, now));

        // A new process reads the same suppression.
        let reopened = History::open(dir.path(), now + 60);
        assert!(reopened.snoozed(KEY, now + 60));
        assert!(reopened.snoozed(KEY, now + SNOOZE_SECS - 1));
        assert!(!reopened.snoozed(KEY, now + SNOOZE_SECS));
        // Another version is not covered.
        assert!(!reopened.snoozed("x86_64-pc-windows-msvc/0.28.0", now + 60));
    }

    #[test]
    fn catalog_reissue_does_not_bypass_version_snooze() {
        // The key carries the version, not the candidate id: a re-issued
        // catalog of the same version produces the same key.
        let dir = TempDir::new("auto-update-reissue");
        let now = test_now();
        let history = History::open(dir.path(), now);
        history.snooze(KEY, now + SNOOZE_SECS).unwrap();
        assert!(history.snoozed(KEY, now + 1));
        assert!(!history.snoozed("x86_64-pc-windows-msvc/0.27.1", now + 1));
        assert!(!history.snoozed("aarch64-apple-darwin/0.27.0", now + 1));
    }

    #[test]
    fn a_snooze_time_far_in_the_future_is_an_anomaly() {
        let dir = TempDir::new("auto-update-anomaly");
        let now = test_now();
        let history = History::open(dir.path(), now);
        history.snooze(KEY, now + 30 * 24 * 3600).unwrap();
        assert!(!history.snoozed(KEY, now));
    }

    #[test]
    fn unreadable_history_defers_startup_prompt() {
        let dir = TempDir::new("auto-update-broken");
        let now = test_now();
        std::fs::write(dir.path().join(HISTORY_FILE), b"not a history").unwrap();
        let history = History::open(dir.path(), now);
        assert!(history.snoozed(KEY, now));
        assert!(history.snoozed("other/1.0.0", now + SNOOZE_SECS - 1));
        assert!(!history.snoozed(KEY, now + SNOOZE_SECS));
        // The first check still follows the normal start delay.
        assert_eq!(history.last_attempt(), None);
    }

    #[test]
    fn broken_history_hold_survives_a_rewrite_and_restart() {
        let dir = TempDir::new("auto-update-hold");
        let now = test_now();
        std::fs::write(dir.path().join(HISTORY_FILE), b"not a history").unwrap();
        let history = History::open(dir.path(), now);
        // A normal write repairs the file; the hold must go with it.
        history.record_attempt(now + 10).unwrap();

        let reopened = History::open(dir.path(), now + 60);
        assert_eq!(reopened.last_attempt(), Some(now + 10));
        assert!(reopened.snoozed(KEY, now + 60));
        assert!(reopened.snoozed(KEY, now + SNOOZE_SECS - 1));
        // It is the original hold (from the first start), not a new one.
        assert!(!reopened.snoozed(KEY, now + SNOOZE_SECS));
    }

    #[test]
    fn busy_skip_does_not_record_an_attempt() {
        // Recording is an explicit call made only for an accepted attempt; a
        // skipped one leaves the history untouched.
        let dir = TempDir::new("auto-update-busy");
        let history = History::open(dir.path(), test_now());
        assert_eq!(history.last_attempt(), None);
        assert!(!dir.path().join(HISTORY_FILE).exists());
    }
}
