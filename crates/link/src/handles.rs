//! Range handles: what Hane remembers about one inserted utterance so a later
//! replacement can be checked against it.

use crate::auth::random_hex;
use crate::message::HANDLE_LIFETIME_MS;
use hane_document::SourceRange;
use std::collections::HashMap;
use std::time::{Duration, Instant};

pub const HANDLE_LIFETIME: Duration = Duration::from_millis(HANDLE_LIFETIME_MS);
const MAX_HANDLES: usize = 64;

/// Everything bound to a handle on the server side.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RangeRecord {
    /// The registered window the insertion happened in.
    pub window: u64,
    pub document_id: String,
    pub session: u64,
    pub generation: u64,
    pub inserted_revision: u64,
    /// The exact inserted text.
    pub text: String,
    pub text_hash: String,
    /// Where the text was inserted, at `inserted_revision`.
    pub range: SourceRange,
    /// The identity of the history step that recorded the insertion.
    pub history_entry: u64,
    expires_at: Option<Instant>,
}

impl RangeRecord {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        window: u64,
        document_id: String,
        session: u64,
        generation: u64,
        inserted_revision: u64,
        text: String,
        text_hash: String,
        range: SourceRange,
        history_entry: u64,
    ) -> Self {
        Self {
            window,
            document_id,
            session,
            generation,
            inserted_revision,
            text,
            text_hash,
            range,
            history_entry,
            expires_at: None,
        }
    }
}

#[derive(Default)]
pub struct HandleTable {
    records: HashMap<String, RangeRecord>,
}

impl HandleTable {
    pub fn len(&self) -> usize {
        self.records.len()
    }

    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    /// Issues an unguessable handle valid for 120 seconds. `None` when the
    /// system random source fails; the insertion is then not offered a handle.
    pub fn issue(&mut self, mut record: RangeRecord, now: Instant) -> Option<String> {
        self.purge(now);
        if self.records.len() >= MAX_HANDLES {
            // The oldest handle goes first; a client holds at most a few.
            let oldest = self
                .records
                .iter()
                .min_by_key(|(_, record)| record.expires_at)
                .map(|(handle, _)| handle.clone());
            if let Some(oldest) = oldest {
                self.records.remove(&oldest);
            }
        }
        record.expires_at = Some(now + HANDLE_LIFETIME);
        let handle = random_hex(16)?;
        self.records.insert(handle.clone(), record);
        Some(handle)
    }

    /// The live record for `handle`. Unknown, expired and consumed handles are
    /// all `None`: the client cannot tell them apart.
    pub fn get(&mut self, handle: &str, now: Instant) -> Option<&RangeRecord> {
        self.purge(now);
        self.records.get(handle)
    }

    /// Uses the handle up.
    pub fn consume(&mut self, handle: &str) -> Option<RangeRecord> {
        self.records.remove(handle)
    }

    /// Drops every handle bound to a window or document that went away.
    pub fn retain(&mut self, mut keep: impl FnMut(&RangeRecord) -> bool) {
        self.records.retain(|_, record| keep(record));
    }

    fn purge(&mut self, now: Instant) {
        self.records
            .retain(|_, record| record.expires_at.is_none_or(|at| at > now));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record() -> RangeRecord {
        RangeRecord::new(
            1,
            "doc".into(),
            2,
            0,
            5,
            "abc".into(),
            "h".into(),
            SourceRange::new(3, 6),
            9,
        )
    }

    #[test]
    fn a_handle_is_opaque_unique_and_expires_after_120_seconds() {
        let mut table = HandleTable::default();
        let now = Instant::now();
        let a = table.issue(record(), now).unwrap();
        let b = table.issue(record(), now).unwrap();
        assert_ne!(a, b);
        assert_eq!(a.len(), 32);
        assert!(
            table
                .get(&a, now + Duration::from_millis(119_999))
                .is_some()
        );
        assert!(table.get(&a, now + HANDLE_LIFETIME).is_none());
    }

    #[test]
    fn consuming_invalidates_the_handle() {
        let mut table = HandleTable::default();
        let now = Instant::now();
        let handle = table.issue(record(), now).unwrap();
        assert!(table.consume(&handle).is_some());
        assert!(table.get(&handle, now).is_none());
        assert!(table.consume(&handle).is_none());
        assert!(table.get("never-issued", now).is_none());
    }

    #[test]
    fn the_table_is_bounded() {
        let mut table = HandleTable::default();
        let now = Instant::now();
        for _ in 0..(MAX_HANDLES + 10) {
            table.issue(record(), now).unwrap();
        }
        assert_eq!(table.len(), MAX_HANDLES);
    }

    #[test]
    fn retain_drops_handles_of_a_gone_document() {
        let mut table = HandleTable::default();
        let now = Instant::now();
        let kept = table.issue(record(), now).unwrap();
        let mut other = record();
        other.session = 77;
        let gone = table.issue(other, now).unwrap();
        table.retain(|record| record.session != 77);
        assert!(table.get(&kept, now).is_some());
        assert!(table.get(&gone, now).is_none());
    }
}
