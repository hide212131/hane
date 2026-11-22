//! The record of mutation requests, so a resend of the same operation is
//! answered from what happened instead of being applied twice.

use crate::message::{ErrorCode, LinkError, LinkResult};
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// A record is kept at least this long after the request was first accepted.
pub const RETENTION: Duration = Duration::from_secs(5 * 60);
pub const DEFAULT_CAPACITY: usize = 512;

#[derive(Debug)]
enum State {
    InProgress,
    Done(LinkResult),
}

#[derive(Debug)]
struct Entry {
    fingerprint: String,
    state: State,
    accepted_at: Instant,
}

/// What [`Ledger::begin`] decided.
#[derive(Debug, Eq, PartialEq)]
pub enum Begin {
    /// A new operation; the caller must apply it and call [`Ledger::finish`].
    Fresh,
    /// The same operation is still being applied.
    InProgress,
    /// The same operation already finished; answer with this.
    Replay(LinkResult),
    /// The id was used before with different content.
    Conflict,
    /// No room for a new record, so the request is refused before it can apply.
    Full,
}

pub struct Ledger {
    entries: HashMap<String, Entry>,
    capacity: usize,
}

impl Default for Ledger {
    fn default() -> Self {
        Self::with_capacity(DEFAULT_CAPACITY)
    }
}

impl Ledger {
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            entries: HashMap::new(),
            capacity,
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn begin(&mut self, operation_id: &str, fingerprint: &str, now: Instant) -> Begin {
        if let Some(entry) = self.entries.get(operation_id) {
            if entry.fingerprint != fingerprint {
                return Begin::Conflict;
            }
            return match &entry.state {
                State::InProgress => Begin::InProgress,
                State::Done(result) => Begin::Replay(result.clone()),
            };
        }
        if self.entries.len() >= self.capacity {
            // Only finished records past the retention window may make room.
            self.entries.retain(|_, entry| {
                !matches!(entry.state, State::Done(_))
                    || now.saturating_duration_since(entry.accepted_at) < RETENTION
            });
            if self.entries.len() >= self.capacity {
                return Begin::Full;
            }
        }
        self.entries.insert(
            operation_id.to_owned(),
            Entry {
                fingerprint: fingerprint.to_owned(),
                state: State::InProgress,
                accepted_at: now,
            },
        );
        Begin::Fresh
    }

    /// Records the outcome of an operation that [`Self::begin`] started.
    pub fn finish(&mut self, operation_id: &str, result: LinkResult) {
        if let Some(entry) = self.entries.get_mut(operation_id) {
            entry.state = State::Done(result);
        }
    }

    /// Drops a record whose request never reached the editor (queue full,
    /// service stopping), so a resend can still be applied.
    pub fn forget(&mut self, operation_id: &str) {
        self.entries.remove(operation_id);
    }
}

/// The error a full ledger answers with: nothing was applied.
pub const FULL_ERROR: LinkError = LinkError::not_applied(ErrorCode::Busy);

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_resend_of_a_finished_operation_replays_the_recorded_result() {
        let mut ledger = Ledger::default();
        let now = Instant::now();
        assert_eq!(ledger.begin("op", "fp", now), Begin::Fresh);
        assert_eq!(ledger.begin("op", "fp", now), Begin::InProgress);
        ledger.finish("op", Ok(json!({"revision": "5"})));
        assert_eq!(
            ledger.begin("op", "fp", now),
            Begin::Replay(Ok(json!({"revision": "5"})))
        );
    }

    #[test]
    fn a_failed_outcome_is_replayed_too() {
        let mut ledger = Ledger::default();
        let now = Instant::now();
        ledger.begin("op", "fp", now);
        let error = LinkError::not_applied(ErrorCode::StaleVersion);
        ledger.finish("op", Err(error));
        assert_eq!(ledger.begin("op", "fp", now), Begin::Replay(Err(error)));
    }

    #[test]
    fn the_same_id_with_other_content_conflicts() {
        let mut ledger = Ledger::default();
        let now = Instant::now();
        ledger.begin("op", "fp", now);
        assert_eq!(ledger.begin("op", "other", now), Begin::Conflict);
        ledger.finish("op", Ok(json!(null)));
        assert_eq!(ledger.begin("op", "other", now), Begin::Conflict);
    }

    #[test]
    fn a_full_ledger_refuses_new_operations_but_not_resends() {
        let mut ledger = Ledger::with_capacity(2);
        let now = Instant::now();
        assert_eq!(ledger.begin("a", "1", now), Begin::Fresh);
        assert_eq!(ledger.begin("b", "2", now), Begin::Fresh);
        ledger.finish("a", Ok(json!(1)));
        ledger.finish("b", Ok(json!(2)));
        assert_eq!(ledger.begin("c", "3", now), Begin::Full);
        assert_eq!(ledger.begin("a", "1", now), Begin::Replay(Ok(json!(1))));
        // Records inside the retention window are never evicted for room.
        assert_eq!(
            ledger.begin("c", "3", now + RETENTION - Duration::from_millis(1)),
            Begin::Full
        );
        assert_eq!(ledger.begin("c", "3", now + RETENTION), Begin::Fresh);
    }

    #[test]
    fn an_in_progress_record_is_never_evicted() {
        let mut ledger = Ledger::with_capacity(1);
        let now = Instant::now();
        ledger.begin("a", "1", now);
        assert_eq!(ledger.begin("b", "2", now + RETENTION * 2), Begin::Full);
    }

    #[test]
    fn forget_lets_a_resend_apply() {
        let mut ledger = Ledger::default();
        let now = Instant::now();
        ledger.begin("op", "fp", now);
        ledger.forget("op");
        assert_eq!(ledger.begin("op", "fp", now), Begin::Fresh);
    }
}
