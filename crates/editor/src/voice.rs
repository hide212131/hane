//! Voice composition: recognized speech shown as unconfirmed segments next to
//! the confirmed text, and committed one segment at a time.
//!
//! The segments never enter the [`hane_document::RopeBuffer`]. Each one only
//! remembers a source offset, followed through every edit by the revision
//! deltas the buffer already keeps, so saving, drafts, search and the history
//! see the confirmed text alone. A commit is an ordinary recorded insertion
//! at the segment's offset: one independent Undo step per segment.
//!
//! Events arrive through [`Editor::voice_apply`] in the `session.start` /
//! `segment.raw` / `segment.polished` / `session.end` shape of the engine
//! contract; the contract checks (continuous event numbers, continuous raw
//! sequence, one polish result per pending raw) live here, not in the caller.

use crate::history::EditKind;
use crate::{Editor, Selection};
use hane_document::{
    Bias, BufferError, Revision, RevisionDelta, SourceOffset, SourceRange, TextBuffer,
};
use std::collections::{BTreeSet, HashSet, VecDeque};
use std::hash::{DefaultHasher, Hash, Hasher};

/// Longest accepted text of one raw or polished segment, in UTF-8 bytes.
pub const MAX_SEGMENT_BYTES: usize = 32 * 1024;
/// Most segments one session may hold.
pub const MAX_SEGMENTS: usize = 512;
/// Events and operations held back while an IME composition is active.
pub const MAX_HELD_ITEMS: usize = 64;
pub const MAX_HELD_BYTES: usize = 2 * 1024 * 1024;
/// How many committed insertions are remembered to order a redo.
const MAX_MARKS: usize = 512;
/// How many accepted events keep a fingerprint for duplicate detection.
const MAX_FINGERPRINTS: usize = 4_096;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum PolishMode {
    Pending,
    Off,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum RawStatus {
    Ok { text: String, polish: PolishMode },
    Silent,
    Failed { code: String },
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum PolishResult {
    Ok(String),
    Failed(String),
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum EndStatus {
    Completed,
    Cancelled,
    Failed { code: String },
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum VoiceEvent {
    SessionStart,
    SegmentRaw { seq: u64, status: RawStatus },
    SegmentPolished { seq: u64, result: PolishResult },
    SessionEnd { status: EndStatus, last_seq: u64 },
}

/// One event as it arrives: the session it belongs to, its number on the
/// wire, and the payload.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct VoiceEnvelope {
    pub session_id: String,
    pub event_no: u64,
    pub event: VoiceEvent,
}

impl VoiceEnvelope {
    fn byte_size(&self) -> usize {
        let text = match &self.event {
            VoiceEvent::SegmentRaw {
                status: RawStatus::Ok { text, .. },
                ..
            }
            | VoiceEvent::SegmentPolished {
                result: PolishResult::Ok(text),
                ..
            } => text.len(),
            _ => 0,
        };
        text + self.session_id.len() + 64
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PolishState {
    /// The polish result has not arrived.
    Waiting,
    /// Polishing is off for this session.
    Off,
    Ready(String),
    Failed(String),
}

/// One unconfirmed segment. It shows exactly one sentence: the raw text at
/// first, replaced in place by the polished text once that arrives.
#[derive(Clone, Debug)]
pub struct VoiceSegment {
    pub seq: u64,
    pub raw: String,
    /// A separator the commit inserts before the text. It belongs to this
    /// segment, so it is shown, committed and undone with it.
    pub lead: &'static str,
    pub polish: PolishState,
    offset: usize,
}

impl VoiceSegment {
    /// The source offset in the confirmed text the segment sits at.
    pub fn offset(&self) -> usize {
        self.offset
    }

    /// The sentence currently shown: the polished text once it arrived,
    /// the raw text until then.
    pub fn shown(&self) -> &str {
        self.polished().unwrap_or(&self.raw)
    }

    /// The text drawn in the body: the separator and the shown sentence.
    pub fn display_text(&self) -> String {
        format!("{}{}", self.lead, self.shown())
    }

    pub fn polished(&self) -> Option<&str> {
        match &self.polish {
            PolishState::Ready(text) => Some(text),
            _ => None,
        }
    }

}

#[derive(Clone, Copy, Debug)]
struct Mark {
    seq: u64,
    start: usize,
    len: usize,
}

#[derive(Clone, Debug)]
enum Held {
    Event(VoiceEnvelope),
    Commit(u64),
    Discard(u64),
    CommitAll,
    DiscardAll,
}

#[derive(Clone, Debug)]
pub struct VoiceComposition {
    session_id: String,
    rev: Revision,
    /// Where the next raw segment is appended: after every segment so far.
    tail: usize,
    segments: Vec<VoiceSegment>,
    next_event_no: u64,
    fingerprints: VecDeque<(u64, u64)>,
    started: bool,
    ended: Option<EndStatus>,
    last_raw_seq: u64,
    /// Raw segments whose polish result is still owed.
    awaiting: BTreeSet<u64>,
    polished_seen: HashSet<u64>,
    resolved: HashSet<u64>,
    failed_segments: usize,
    marks: Vec<Mark>,
    held: VecDeque<Held>,
    held_bytes: usize,
    last_char: Option<char>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IgnoreReason {
    WrongSession,
    AfterEnd,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VoiceApply {
    Applied,
    /// The same event number with the same content: not applied twice.
    Duplicate,
    /// An IME composition is active; applied when it ends.
    Held,
    Ignored(IgnoreReason),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum VoiceError {
    NoSession,
    Busy,
    UnknownSegment(u64),
    /// The engine broke the contract; the session was dropped.
    Contract(&'static str),
    /// The positions could no longer be followed; the session was dropped.
    Lost,
    Buffer(BufferError),
}

impl From<BufferError> for VoiceError {
    fn from(error: BufferError) -> Self {
        Self::Buffer(error)
    }
}

/// How committed insertions moved offsets, in the order they were applied.
#[derive(Clone, Debug, Default)]
pub struct OffsetMap(Vec<(usize, usize)>);

impl OffsetMap {
    pub fn map(&self, offset: usize) -> usize {
        self.0
            .iter()
            .fold(offset, |x, &(at, len)| if x > at { x + len } else { x })
    }

    pub fn map_range(&self, range: SourceRange) -> SourceRange {
        SourceRange::new(self.map(range.start.0), self.map(range.end.0))
    }

    pub fn map_selection(&self, selection: Selection) -> Selection {
        Selection {
            anchor: SourceOffset(self.map(selection.anchor.0)),
            active: SourceOffset(self.map(selection.active.0)),
        }
    }

    fn push(&mut self, at: usize, len: usize) {
        self.0.push((at, len));
    }
}

fn fingerprint(event: &VoiceEvent) -> u64 {
    let mut hasher = DefaultHasher::new();
    event.hash(&mut hasher);
    hasher.finish()
}

fn needs_space(before: Option<char>, after: Option<char>) -> bool {
    matches!((before, after), (Some(a), Some(b)) if a.is_ascii_alphanumeric() && b.is_ascii_alphanumeric())
}

enum Accepted {
    Applied,
    Duplicate,
    Ignored(IgnoreReason),
    /// `session.end` with `cancelled` or `failed`: drop everything unconfirmed.
    Terminated(Option<String>),
}

impl VoiceComposition {
    fn new(session_id: String, rev: Revision, tail: usize) -> Self {
        Self {
            session_id,
            rev,
            tail,
            segments: Vec::new(),
            next_event_no: 1,
            fingerprints: VecDeque::new(),
            started: false,
            ended: None,
            last_raw_seq: 0,
            awaiting: BTreeSet::new(),
            polished_seen: HashSet::new(),
            resolved: HashSet::new(),
            failed_segments: 0,
            marks: Vec::new(),
            held: VecDeque::new(),
            held_bytes: 0,
            last_char: None,
        }
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// Whether `offset` lies in the stretch this session works on: from the
    /// start of its confirmed text (or the segment's anchor) to its tail.
    pub fn region_contains(&self, anchor: usize, offset: usize) -> bool {
        let start = self
            .marks
            .iter()
            .map(|mark| mark.start)
            .fold(anchor, usize::min);
        (start..=self.tail.max(anchor)).contains(&offset)
    }

    /// The unconfirmed segments in `seq` order.
    pub fn segments(&self) -> &[VoiceSegment] {
        &self.segments
    }

    pub fn ended(&self) -> Option<&EndStatus> {
        self.ended.as_ref()
    }

    /// Whether the recording is still running (no `session.end` yet).
    pub fn is_recording(&self) -> bool {
        self.ended.is_none()
    }

    /// Raw segments the engine reported as failed (no text to show).
    pub fn failed_segments(&self) -> usize {
        self.failed_segments
    }

    pub fn held_count(&self) -> usize {
        self.held.len()
    }

    /// Whether `seq` was shown and has since been committed or discarded.
    pub fn is_resolved(&self, seq: u64) -> bool {
        self.resolved.contains(&seq)
    }

    fn follow(&mut self, deltas: &[RevisionDelta]) {
        for delta in deltas {
            let before = delta.edited_source_range_before;
            let after = delta.edited_source_range_after;
            let insertion = before.is_empty().then_some(after);
            for segment in &mut self.segments {
                let stays = insertion.is_some_and(|inserted| {
                    segment.offset == inserted.start.0
                        && self.marks.iter().any(|mark| {
                            mark.start == inserted.start.0
                                && mark.len == inserted.len_bytes()
                                && mark.seq > segment.seq
                        })
                });
                if !stays {
                    segment.offset = delta
                        .transform_offset(SourceOffset(segment.offset), Bias::After)
                        .map_or(segment.offset, |offset| offset.0);
                }
            }
            self.tail = delta
                .transform_offset(SourceOffset(self.tail), Bias::After)
                .map_or(self.tail, |offset| offset.0);
            self.marks.retain_mut(|mark| {
                let range = SourceRange::new(mark.start, mark.start + mark.len);
                if before == range && after.is_empty() {
                    // Undo of this very commit: keep where it sat for Redo.
                    return true;
                }
                if before.is_empty() {
                    let at = before.start.0;
                    if at > range.start.0 && at < range.end.0 {
                        return false;
                    }
                } else if before.intersects(range) {
                    return false;
                }
                match delta.transform_offset(SourceOffset(mark.start), Bias::Before) {
                    Some(start) => {
                        mark.start = start.0;
                        true
                    }
                    None => false,
                }
            });
        }
    }

    fn accept(
        &mut self,
        envelope: &VoiceEnvelope,
        char_before_tail: impl FnOnce(usize) -> Option<char>,
    ) -> Result<Accepted, &'static str> {
        if envelope.session_id != self.session_id {
            return Ok(Accepted::Ignored(IgnoreReason::WrongSession));
        }
        if self.ended.is_some() {
            return Ok(Accepted::Ignored(IgnoreReason::AfterEnd));
        }
        let print = fingerprint(&envelope.event);
        if let Some((_, old)) = self
            .fingerprints
            .iter()
            .find(|(number, _)| *number == envelope.event_no)
        {
            return if *old == print {
                Ok(Accepted::Duplicate)
            } else {
                Err("same event number with different content")
            };
        }
        if envelope.event_no != self.next_event_no {
            return Err("event number is not continuous");
        }
        let outcome = self.accept_event(&envelope.event, char_before_tail)?;
        self.next_event_no += 1;
        self.fingerprints.push_back((envelope.event_no, print));
        if self.fingerprints.len() > MAX_FINGERPRINTS {
            self.fingerprints.pop_front();
        }
        Ok(outcome)
    }

    fn accept_event(
        &mut self,
        event: &VoiceEvent,
        char_before_tail: impl FnOnce(usize) -> Option<char>,
    ) -> Result<Accepted, &'static str> {
        match event {
            VoiceEvent::SessionStart => {
                if self.started || self.next_event_no != 1 {
                    return Err("session.start must be the first event");
                }
                self.started = true;
                Ok(Accepted::Applied)
            }
            VoiceEvent::SegmentRaw { seq, status } => {
                if !self.started {
                    return Err("segment.raw before session.start");
                }
                if *seq != self.last_raw_seq + 1 {
                    return Err("raw sequence is not continuous");
                }
                match status {
                    RawStatus::Ok { text, polish } => {
                        if text.is_empty() || text.len() > MAX_SEGMENT_BYTES {
                            return Err("raw text is empty or too long");
                        }
                        if self.segments.len() + self.resolved.len() >= MAX_SEGMENTS {
                            return Err("too many segments");
                        }
                        let before = self
                            .last_char
                            .or_else(|| char_before_tail(self.tail));
                        let lead = if needs_space(before, text.chars().next()) {
                            " "
                        } else {
                            ""
                        };
                        self.last_char = text.chars().next_back();
                        let polish = match polish {
                            PolishMode::Pending => {
                                self.awaiting.insert(*seq);
                                PolishState::Waiting
                            }
                            PolishMode::Off => PolishState::Off,
                        };
                        self.segments.push(VoiceSegment {
                            seq: *seq,
                            raw: text.clone(),
                            lead,
                            polish,
                            offset: self.tail,
                        });
                    }
                    RawStatus::Silent => {}
                    RawStatus::Failed { .. } => self.failed_segments += 1,
                }
                self.last_raw_seq = *seq;
                Ok(Accepted::Applied)
            }
            VoiceEvent::SegmentPolished { seq, result } => {
                if !self.started {
                    return Err("segment.polished before session.start");
                }
                if !self.awaiting.remove(seq) {
                    return Err(if self.polished_seen.contains(seq) {
                        "polish result sent twice"
                    } else {
                        "polish result without a pending raw segment"
                    });
                }
                self.polished_seen.insert(*seq);
                let state = match result {
                    PolishResult::Ok(text) => {
                        if text.is_empty() || text.len() > MAX_SEGMENT_BYTES {
                            return Err("polished text is empty or too long");
                        }
                        PolishState::Ready(text.clone())
                    }
                    PolishResult::Failed(code) => PolishState::Failed(code.clone()),
                };
                // A segment already confirmed or discarded takes no polished text.
                if let Some(segment) = self.segments.iter_mut().find(|s| s.seq == *seq) {
                    segment.polish = state;
                }
                Ok(Accepted::Applied)
            }
            VoiceEvent::SessionEnd { status, last_seq } => {
                if *last_seq != self.last_raw_seq {
                    return Err("lastSeq does not match the last raw segment");
                }
                if *status == EndStatus::Completed && !self.awaiting.is_empty() {
                    return Err("session completed with an unfinished polish");
                }
                self.ended = Some(status.clone());
                match status {
                    EndStatus::Completed => Ok(Accepted::Applied),
                    EndStatus::Cancelled => Ok(Accepted::Terminated(None)),
                    EndStatus::Failed { code } => Ok(Accepted::Terminated(Some(code.clone()))),
                }
            }
        }
    }
}

impl Editor {
    /// Starts a voice session that appends at the caret. The id is the one
    /// Hane issued for this recording; events carrying another id are
    /// ignored.
    pub fn voice_begin(&mut self, session_id: impl Into<String>) -> Result<(), VoiceError> {
        if self.voice.is_some() {
            return Err(VoiceError::Busy);
        }
        self.voice_notice = None;
        self.voice = Some(VoiceComposition::new(
            session_id.into(),
            self.document.revision(),
            self.selection.active.0,
        ));
        Ok(())
    }

    pub fn voice(&self) -> Option<&VoiceComposition> {
        self.voice.as_ref()
    }

    /// Why the last session was dropped, until the next one starts.
    pub fn voice_notice(&self) -> Option<&str> {
        self.voice_notice.as_deref()
    }

    /// Drops the session and everything unconfirmed. Confirmed text stays.
    pub fn voice_cancel(&mut self) {
        self.voice = None;
    }

    fn voice_drop(&mut self, notice: &str) {
        self.voice = None;
        self.voice_notice = Some(notice.to_owned());
    }

    /// Brings every segment offset up to the current revision. A history that
    /// can no longer be followed drops the session.
    pub fn voice_sync(&mut self) {
        let Some(voice) = self.voice.as_mut() else {
            return;
        };
        let revision = self.document.revision();
        if voice.rev == revision {
            return;
        }
        match self.document.deltas_since(voice.rev) {
            Ok(deltas) => {
                voice.follow(&deltas);
                voice.rev = revision;
            }
            Err(_) => self.voice_drop("位置を追えなくなったため、音声の未確定分を破棄しました"),
        }
    }

    /// Feeds one engine event in. While an IME composition is active the body
    /// is not changed: the event is held and applied when it ends.
    pub fn voice_apply(&mut self, envelope: VoiceEnvelope) -> Result<VoiceApply, VoiceError> {
        if self.voice.is_none() {
            return Err(VoiceError::NoSession);
        }
        if self.ime.is_some() {
            let size = envelope.byte_size();
            return self.voice_hold(Held::Event(envelope), size);
        }
        self.voice_apply_now(envelope)
    }

    fn voice_hold(&mut self, held: Held, size: usize) -> Result<VoiceApply, VoiceError> {
        let voice = self.voice.as_mut().ok_or(VoiceError::NoSession)?;
        voice.held_bytes += size;
        voice.held.push_back(held);
        if voice.held.len() > MAX_HELD_ITEMS || voice.held_bytes > MAX_HELD_BYTES {
            self.voice_drop("受信待ちが上限を超えたため、音声の未確定分を破棄しました");
            return Err(VoiceError::Contract("held queue overflow"));
        }
        Ok(VoiceApply::Held)
    }

    fn voice_apply_now(&mut self, envelope: VoiceEnvelope) -> Result<VoiceApply, VoiceError> {
        self.voice_sync();
        let document = &self.document;
        let voice = self.voice.as_mut().ok_or(VoiceError::Lost)?;
        let accepted = voice.accept(&envelope, |tail| {
            let start = previous_char_start(document, tail)?;
            let text = document
                .text(SourceRange::new(start, tail))
                .ok()?;
            text.chars().next_back()
        });
        match accepted {
            Ok(Accepted::Applied) => {
                let finished = voice.ended == Some(EndStatus::Completed)
                    && voice.segments.is_empty();
                if finished {
                    self.voice = None;
                }
                Ok(VoiceApply::Applied)
            }
            Ok(Accepted::Duplicate) => Ok(VoiceApply::Duplicate),
            Ok(Accepted::Ignored(reason)) => Ok(VoiceApply::Ignored(reason)),
            Ok(Accepted::Terminated(code)) => {
                match code {
                    Some(code) => self.voice_drop(&format!(
                        "音声入力が失敗したため、未確定分を破棄しました（{code}）"
                    )),
                    None => self.voice = None,
                }
                Ok(VoiceApply::Applied)
            }
            Err(message) => {
                self.voice_drop("音声入力の契約違反のため、未確定分を破棄しました");
                Err(VoiceError::Contract(message))
            }
        }
    }

    /// Applies what was held while an IME composition was active. Called when
    /// the composition ends.
    pub fn voice_flush_held(&mut self) {
        while self.ime.is_none() {
            let Some(voice) = self.voice.as_mut() else {
                return;
            };
            let Some(held) = voice.held.pop_front() else {
                return;
            };
            voice.held_bytes = voice.held.iter().fold(0, |sum, item| {
                sum + match item {
                    Held::Event(event) => event.byte_size(),
                    _ => 0,
                }
            });
            let _ = match held {
                Held::Event(event) => self.voice_apply_now(event).map(|_| ()),
                Held::Commit(seq) => self.voice_commit_now(seq, true).map(|_| ()),
                Held::Discard(seq) => self.voice_discard_now(seq),
                Held::CommitAll => self.voice_commit_all_now(),
                Held::DiscardAll => self.voice_discard_all_now(),
            };
        }
    }

    /// Commits one segment with the sentence it currently shows.
    /// One undoable insertion; the caret moves to the end of the sentence.
    pub fn voice_commit(&mut self, seq: u64) -> Result<(), VoiceError> {
        self.voice_sync();
        let voice = self.voice.as_ref().ok_or(VoiceError::NoSession)?;
        if !voice.segments.iter().any(|segment| segment.seq == seq) {
            return Err(VoiceError::UnknownSegment(seq));
        }
        if self.ime.is_some() {
            self.voice_hold(Held::Commit(seq), 0)?;
            return Ok(());
        }
        self.voice_commit_now(seq, true).map(|_| ())
    }

    pub fn voice_discard(&mut self, seq: u64) -> Result<(), VoiceError> {
        self.voice_sync();
        let voice = self.voice.as_ref().ok_or(VoiceError::NoSession)?;
        if !voice.segments.iter().any(|segment| segment.seq == seq) {
            return Err(VoiceError::UnknownSegment(seq));
        }
        if self.ime.is_some() {
            self.voice_hold(Held::Discard(seq), 0)?;
            return Ok(());
        }
        self.voice_discard_now(seq)
    }

    /// Commits every segment in `seq` order with the sentence it currently
    /// shows. Each is its own history step.
    pub fn voice_commit_all(&mut self) -> Result<(), VoiceError> {
        self.voice_sync();
        if self.voice.is_none() {
            return Err(VoiceError::NoSession);
        }
        if self.ime.is_some() {
            self.voice_hold(Held::CommitAll, 0)?;
            return Ok(());
        }
        self.voice_commit_all_now()
    }

    pub fn voice_discard_all(&mut self) -> Result<(), VoiceError> {
        self.voice_sync();
        if self.voice.is_none() {
            return Err(VoiceError::NoSession);
        }
        if self.ime.is_some() {
            self.voice_hold(Held::DiscardAll, 0)?;
            return Ok(());
        }
        self.voice_discard_all_now()
    }

    fn voice_commit_all_now(&mut self) -> Result<(), VoiceError> {
        loop {
            let Some(segment) = self
                .voice
                .as_ref()
                .and_then(|voice| voice.segments.first())
            else {
                return Ok(());
            };
            let seq = segment.seq;
            self.voice_commit_now(seq, true)?;
        }
    }

    fn voice_discard_all_now(&mut self) -> Result<(), VoiceError> {
        if let Some(voice) = self.voice.as_mut() {
            voice.segments.clear();
            self.voice_release_if_finished();
        }
        Ok(())
    }

    fn voice_discard_now(&mut self, seq: u64) -> Result<(), VoiceError> {
        self.voice_sync();
        let voice = self.voice.as_mut().ok_or(VoiceError::NoSession)?;
        let index = voice
            .segments
            .iter()
            .position(|segment| segment.seq == seq)
            .ok_or(VoiceError::UnknownSegment(seq))?;
        voice.segments.remove(index);
        voice.resolved.insert(seq);
        self.voice_release_if_finished();
        Ok(())
    }

    fn voice_release_if_finished(&mut self) {
        if self
            .voice
            .as_ref()
            .is_some_and(|voice| voice.segments.is_empty() && voice.ended.is_some())
        {
            self.voice = None;
        }
    }

    /// The insertion itself. The history step, selection and every other
    /// segment's offset are settled here, in one go. With `move_caret` the
    /// caret goes to the end of the confirmed sentence, as when an IME commits;
    /// without it the selection only follows the text (the edit that caused
    /// the confirmation still needs the selection it was issued against).
    fn voice_commit_now(&mut self, seq: u64, move_caret: bool) -> Result<OffsetMap, VoiceError> {
        self.voice_sync();
        let voice = self.voice.as_ref().ok_or(VoiceError::NoSession)?;
        let segment = voice
            .segments
            .iter()
            .find(|segment| segment.seq == seq)
            .ok_or(VoiceError::UnknownSegment(seq))?;
        let text = segment.display_text();
        let at = segment.offset;
        let len = text.len();
        let selection_before = self.selection;
        self.history.break_group();
        let summary = self.document.edit(SourceRange::empty(at), &text)?;
        let mut map = OffsetMap::default();
        map.push(at, len);
        let selection_after = if move_caret {
            Selection::caret(SourceOffset(at + len))
        } else {
            map.map_selection(selection_before)
        };
        self.selection = selection_after;
        self.preferred_grapheme_column = None;
        self.preferred_visual_x = None;
        self.history.record_replacement(
            at,
            summary.inverse.replacement.clone(),
            text,
            selection_before,
            selection_after,
            EditKind::Replace,
        );
        let revision = self.document.revision();
        if let Some(voice) = self.voice.as_mut() {
            voice.rev = revision;
            voice.segments.retain(|segment| segment.seq != seq);
            for other in &mut voice.segments {
                if other.offset > at || (other.offset == at && other.seq > seq) {
                    other.offset += len;
                }
            }
            if voice.tail >= at {
                voice.tail += len;
            }
            for mark in &mut voice.marks {
                if mark.start > at {
                    mark.start += len;
                }
            }
            voice.marks.push(Mark { seq, start: at, len });
            if voice.marks.len() > MAX_MARKS {
                voice.marks.remove(0);
            }
            voice.resolved.insert(seq);
        }
        self.voice_release_if_finished();
        Ok(map)
    }

    /// Before a user edit of `range`: commits, in `seq` order and with the
    /// sentence each shows, every segment the edit reaches; a polished text
    /// arriving later finds no segment and is dropped. A
    /// segment sits at one source offset, so a range reaches it when it
    /// covers the offset or begins exactly there. Returns how offsets moved so
    /// the caller can carry its own ranges to the new text.
    pub(crate) fn voice_guard(&mut self, range: SourceRange) -> Result<OffsetMap, BufferError> {
        let mut map = OffsetMap::default();
        if self.voice.is_none() || range.is_empty() || self.ime.is_some() {
            return Ok(map);
        }
        self.voice_sync();
        let Some(voice) = self.voice.as_ref() else {
            return Ok(map);
        };
        let mut touched: Vec<u64> = voice
            .segments
            .iter()
            .filter(|segment| {
                let offset = segment.offset;
                offset == range.start.0 || (offset > range.start.0 && offset < range.end.0)
            })
            .map(|segment| segment.seq)
            .collect();
        touched.sort_unstable();
        for seq in touched {
            match self.voice_commit_now(seq, false) {
                Ok(step) => map.0.extend(step.0),
                Err(VoiceError::Buffer(error)) => return Err(error),
                Err(_) => {}
            }
        }
        Ok(map)
    }

    /// Before a forward delete from an empty selection: segments sitting at
    /// the caret are the text the key would eat, so they commit first.
    pub(crate) fn voice_guard_caret(&mut self) -> Result<(), BufferError> {
        if self.voice.is_none() || self.ime.is_some() || !self.selection.range().is_empty() {
            return Ok(());
        }
        self.voice_sync();
        let at = self.selection.active.0;
        let Some(voice) = self.voice.as_ref() else {
            return Ok(());
        };
        let touched: Vec<u64> = voice
            .segments
            .iter()
            .filter(|segment| segment.offset == at)
            .map(|segment| segment.seq)
            .collect();
        for seq in touched {
            match self.voice_commit_now(seq, false) {
                Ok(_) => {}
                Err(VoiceError::Buffer(error)) => return Err(error),
                Err(_) => {}
            }
        }
        Ok(())
    }
}

fn previous_char_start(document: &hane_document::RopeBuffer, offset: usize) -> Option<usize> {
    let mut start = offset.checked_sub(1)?;
    // A char is at most four bytes; walk back to its first byte.
    for _ in 0..4 {
        if document
            .text(SourceRange::new(start, offset))
            .is_ok()
        {
            return Some(start);
        }
        start = start.checked_sub(1)?;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const SESSION: &str = "a162d4cb5747474d9cf7fcaf1833dabb";

    struct Feed {
        next: u64,
    }

    impl Feed {
        fn new() -> Self {
            Self { next: 1 }
        }

        fn send(&mut self, editor: &mut Editor, event: VoiceEvent) -> Result<VoiceApply, VoiceError> {
            let envelope = VoiceEnvelope {
                session_id: SESSION.to_owned(),
                event_no: self.next,
                event,
            };
            self.next += 1;
            editor.voice_apply(envelope)
        }

        fn start(&mut self, editor: &mut Editor) {
            self.send(editor, VoiceEvent::SessionStart).unwrap();
        }

        fn raw(&mut self, editor: &mut Editor, seq: u64, text: &str) {
            self.send(
                editor,
                VoiceEvent::SegmentRaw {
                    seq,
                    status: RawStatus::Ok {
                        text: text.to_owned(),
                        polish: PolishMode::Pending,
                    },
                },
            )
            .unwrap();
        }

        fn polished(&mut self, editor: &mut Editor, seq: u64, text: &str) {
            self.send(
                editor,
                VoiceEvent::SegmentPolished {
                    seq,
                    result: PolishResult::Ok(text.to_owned()),
                },
            )
            .unwrap();
        }
    }

    fn editor_with(text: &str, caret: usize) -> Editor {
        let mut editor = Editor::new(text);
        editor
            .set_selection(Selection::caret(SourceOffset(caret)))
            .unwrap();
        editor.voice_begin(SESSION).unwrap();
        editor
    }

    fn text(editor: &Editor) -> String {
        editor.document().full_text()
    }

    fn pending(editor: &mut Editor) -> Vec<(u64, usize, String)> {
        editor.voice_sync();
        editor
            .voice()
            .map(|voice| {
                voice
                    .segments()
                    .iter()
                    .map(|s| (s.seq, s.offset(), s.display_text()))
                    .collect()
            })
            .unwrap_or_default()
    }

    #[test]
    fn raw_text_stays_out_of_the_document_and_the_history() {
        let mut editor = editor_with("前後", 3);
        let mut feed = Feed::new();
        feed.start(&mut editor);
        feed.raw(&mut editor, 1, "えーと");
        feed.raw(&mut editor, 2, "原文です");
        assert_eq!(text(&editor), "前後");
        assert!(!editor.can_undo());
        assert_eq!(
            pending(&mut editor),
            vec![(1, 3, "えーと".into()), (2, 3, "原文です".into())]
        );
        feed.polished(&mut editor, 2, "整文です");
        assert_eq!(text(&editor), "前後");
        assert_eq!(editor.document().revision(), Revision(0));
        assert_eq!(
            editor.voice().unwrap().segments()[1].polished(),
            Some("整文です")
        );
        // The body side keeps the raw text.
        assert_eq!(editor.voice().unwrap().segments()[1].raw, "原文です");
    }

    #[test]
    fn segments_commit_out_of_order_in_one_undo_step_each() {
        let mut editor = editor_with("", 0);
        let mut feed = Feed::new();
        feed.start(&mut editor);
        feed.raw(&mut editor, 1, "一つ目。");
        feed.raw(&mut editor, 2, "二つ目。");
        feed.polished(&mut editor, 2, "二つ目、整文。");
        feed.polished(&mut editor, 1, "一つ目、整文。");
        editor.voice_commit(2).unwrap();
        // The earlier segment stays in front of the confirmed later one.
        assert_eq!(text(&editor), "二つ目、整文。");
        assert_eq!(pending(&mut editor), vec![(1, 0, "一つ目、整文。".into())]);
        feed.raw(&mut editor, 3, "三つ目。");
        assert_eq!(
            pending(&mut editor),
            vec![(1, 0, "一つ目、整文。".into()), (3, 21, "三つ目。".into())]
        );
        editor.voice_commit(1).unwrap();
        assert_eq!(text(&editor), "一つ目、整文。二つ目、整文。");
        feed.send(
            &mut editor,
            VoiceEvent::SegmentPolished {
                seq: 3,
                result: PolishResult::Ok("三つ目、整文。".into()),
            },
        )
        .unwrap();
        editor.voice_commit(3).unwrap();
        assert_eq!(text(&editor), "一つ目、整文。二つ目、整文。三つ目、整文。");
        // Each commit is its own Undo step, newest confirmation first.
        editor.dispatch(crate::EditorCommand::Undo).unwrap();
        assert_eq!(text(&editor), "一つ目、整文。二つ目、整文。");
        editor.dispatch(crate::EditorCommand::Undo).unwrap();
        assert_eq!(text(&editor), "二つ目、整文。");
        editor.dispatch(crate::EditorCommand::Undo).unwrap();
        assert_eq!(text(&editor), "");
        // Undo never brings back unconfirmed text.
        assert!(pending(&mut editor).is_empty());
        editor.dispatch(crate::EditorCommand::Redo).unwrap();
        editor.dispatch(crate::EditorCommand::Redo).unwrap();
        editor.dispatch(crate::EditorCommand::Redo).unwrap();
        assert_eq!(text(&editor), "一つ目、整文。二つ目、整文。三つ目、整文。");
        assert!(pending(&mut editor).is_empty());
    }

    #[test]
    fn redo_keeps_the_segment_order_of_the_original_commit() {
        let mut editor = editor_with("", 0);
        let mut feed = Feed::new();
        feed.start(&mut editor);
        feed.raw(&mut editor, 1, "あ");
        feed.raw(&mut editor, 2, "い");
        editor.voice_commit(2).unwrap();
        editor.dispatch(crate::EditorCommand::Undo).unwrap();
        assert_eq!(text(&editor), "");
        assert_eq!(pending(&mut editor), vec![(1, 0, "あ".into())]);
        editor.dispatch(crate::EditorCommand::Redo).unwrap();
        assert_eq!(text(&editor), "い");
        // The unconfirmed earlier segment is still in front of the redone text.
        assert_eq!(pending(&mut editor), vec![(1, 0, "あ".into())]);
    }

    #[test]
    fn a_polished_text_replaces_the_shown_sentence_in_place() {
        let mut editor = editor_with("", 0);
        let mut feed = Feed::new();
        feed.start(&mut editor);
        feed.raw(&mut editor, 1, "あ");
        feed.raw(&mut editor, 2, "い");
        assert_eq!(pending(&mut editor)[0].2, "あ");
        feed.polished(&mut editor, 1, "亜");
        assert_eq!(pending(&mut editor)[0].2, "亜");
        // Confirming all uses what each segment shows: polished or raw.
        editor.voice_commit_all().unwrap();
        assert_eq!(text(&editor), "亜い");
    }

    #[test]
    fn a_late_polish_for_a_committed_segment_is_not_applied() {
        let mut editor = editor_with("", 0);
        let mut feed = Feed::new();
        feed.start(&mut editor);
        feed.raw(&mut editor, 1, "あ");
        feed.raw(&mut editor, 2, "い");
        editor.voice_commit(1).unwrap();
        feed.polished(&mut editor, 1, "亜");
        assert_eq!(text(&editor), "あ");
        assert!(editor.voice().unwrap().is_resolved(1));
        editor.dispatch(crate::EditorCommand::Undo).unwrap();
        assert_eq!(text(&editor), "");
        assert_eq!(pending(&mut editor), vec![(2, 0, "い".into())]);
    }

    #[test]
    fn typing_at_the_boundary_goes_in_front_of_the_voice_text() {
        let mut editor = editor_with("ab", 1);
        let mut feed = Feed::new();
        feed.start(&mut editor);
        feed.raw(&mut editor, 1, "音声");
        editor.insert_text("X").unwrap();
        assert_eq!(text(&editor), "aXb");
        assert_eq!(pending(&mut editor), vec![(1, 2, "音声".into())]);
        feed.raw(&mut editor, 2, "続き");
        assert_eq!(
            pending(&mut editor),
            vec![(1, 2, "音声".into()), (2, 2, "続き".into())]
        );
    }

    #[test]
    fn an_edit_outside_the_segments_leaves_them_unconfirmed_and_following() {
        let mut editor = editor_with("頭と尻", "頭と".len());
        let mut feed = Feed::new();
        feed.start(&mut editor);
        feed.raw(&mut editor, 1, "音声");
        editor
            .replace_range_recorded(
                SourceRange::new(0, 3),
                "🙂🙂",
                Selection::caret(SourceOffset(8)),
            )
            .unwrap();
        assert_eq!(text(&editor), "🙂🙂と尻");
        assert_eq!(pending(&mut editor), vec![(1, 11, "音声".into())]);
        assert_eq!(editor.document().revision(), Revision(1));
    }

    #[test]
    fn typing_inside_the_span_confirms_the_shown_text_first() {
        // "ab" + [seg] + "cd": a replace across the boundary reaches the segment.
        let mut editor = editor_with("abcd", 2);
        let mut feed = Feed::new();
        feed.start(&mut editor);
        feed.raw(&mut editor, 1, "音声");
        feed.polished(&mut editor, 1, "整文");
        editor
            .set_selection(Selection {
                anchor: SourceOffset(1),
                active: SourceOffset(3),
            })
            .unwrap();
        editor.insert_text("Z").unwrap();
        // The shown (polished) text was confirmed, then swallowed by the
        // replacement of the selected span.
        assert_eq!(text(&editor), "aZd");
        assert!(pending(&mut editor).is_empty());
        // Undo 1: the replacement is undone, the shown text is back.
        editor.dispatch(crate::EditorCommand::Undo).unwrap();
        assert_eq!(text(&editor), "ab整文cd");
        // Undo 2: the segment's own confirmation.
        editor.dispatch(crate::EditorCommand::Undo).unwrap();
        assert_eq!(text(&editor), "abcd");
        assert!(!editor.can_undo());
    }

    #[test]
    fn deleting_across_two_segments_confirms_both_in_seq_order() {
        let mut editor = editor_with("甲乙丙", "甲".len());
        let mut feed = Feed::new();
        feed.start(&mut editor);
        feed.raw(&mut editor, 1, "あ");
        feed.raw(&mut editor, 2, "い");
        // Move the second segment's offset by confirming nothing; both sit at 3.
        editor
            .set_selection(Selection {
                anchor: SourceOffset(0),
                active: SourceOffset("甲乙".len()),
            })
            .unwrap();
        editor.dispatch(crate::EditorCommand::Backspace).unwrap();
        assert_eq!(text(&editor), "丙");
        editor.dispatch(crate::EditorCommand::Undo).unwrap();
        assert_eq!(text(&editor), "甲乙丙".replace('乙', "あい乙"));
    }

    #[test]
    fn forward_delete_at_the_caret_confirms_the_segment_in_front_of_it() {
        let mut editor = editor_with("xyz", 1);
        let mut feed = Feed::new();
        feed.start(&mut editor);
        feed.raw(&mut editor, 1, "音声");
        editor.dispatch(crate::EditorCommand::Delete).unwrap();
        // The key ate the first grapheme of the (now confirmed) raw text.
        assert_eq!(text(&editor), "x声yz");
        assert!(pending(&mut editor).is_empty());
        editor.dispatch(crate::EditorCommand::Undo).unwrap();
        assert_eq!(text(&editor), "x音声yz");
        editor.dispatch(crate::EditorCommand::Undo).unwrap();
        assert_eq!(text(&editor), "xyz");
    }

    #[test]
    fn backspace_at_the_caret_does_not_touch_the_segment() {
        let mut editor = editor_with("xyz", 2);
        let mut feed = Feed::new();
        feed.start(&mut editor);
        feed.raw(&mut editor, 1, "音声");
        editor.dispatch(crate::EditorCommand::Backspace).unwrap();
        assert_eq!(text(&editor), "xz");
        assert_eq!(pending(&mut editor), vec![(1, 1, "音声".into())]);
    }

    #[test]
    fn a_paste_over_a_selection_reaching_the_segment_confirms_it_first() {
        let mut editor = editor_with("頭\r\n尻", "頭\r\n".len());
        let mut feed = Feed::new();
        feed.start(&mut editor);
        feed.raw(&mut editor, 1, "音声");
        editor
            .replace_range_recorded(
                SourceRange::new(0, text(&editor).len()),
                "全部",
                Selection::caret(SourceOffset(6)),
            )
            .unwrap();
        assert_eq!(text(&editor), "全部");
        assert!(pending(&mut editor).is_empty());
        editor.dispatch(crate::EditorCommand::Undo).unwrap();
        assert_eq!(text(&editor), "頭\r\n音声尻");
    }

    #[test]
    fn composition_start_over_a_segment_confirms_it_but_a_far_one_does_not() {
        let mut editor = editor_with("abcdef", 3);
        let mut feed = Feed::new();
        feed.start(&mut editor);
        feed.raw(&mut editor, 1, "音声");
        // A composition elsewhere keeps the voice text unconfirmed.
        editor.replace_and_mark_text(Some(0..1), "か", None).unwrap();
        // The composition text is in the document, so the segment moved with it.
        assert_eq!(pending(&mut editor), vec![(1, 5, "音声".into())]);
        feed.raw(&mut editor, 2, "続き");
        assert_eq!(editor.voice().unwrap().held_count(), 1);
        assert_eq!(pending(&mut editor).len(), 1);
        editor.commit_text(None, "か").unwrap();
        // The held event is applied once the composition has ended.
        assert_eq!(pending(&mut editor).len(), 2);
        // A composition that starts on the segment's boundary-covering range
        // confirms it before the OS range is converted.
        // OS range 1..5 (UTF-16) of "かbcdef" is "bcde", which spans offset 5.
        editor.replace_and_mark_text(Some(1..5), "あ", None).unwrap();
        assert!(pending(&mut editor).iter().all(|(seq, ..)| *seq != 1));
        assert_eq!(text(&editor), "かあf");
    }

    #[test]
    fn operations_during_a_composition_wait_for_its_end() {
        let mut editor = editor_with("ab", 2);
        let mut feed = Feed::new();
        feed.start(&mut editor);
        feed.raw(&mut editor, 1, "音声");
        editor.replace_and_mark_text(Some(0..1), "か", None).unwrap();
        editor.voice_commit(1).unwrap();
        assert_eq!(pending(&mut editor).len(), 1);
        editor.commit_composition();
        assert!(pending(&mut editor).is_empty());
        assert_eq!(text(&editor), "か" .to_owned() + "b音声");
    }

    #[test]
    fn cancelling_keeps_confirmed_text_and_drops_the_rest() {
        let mut editor = editor_with("", 0);
        let mut feed = Feed::new();
        feed.start(&mut editor);
        feed.raw(&mut editor, 1, "あ");
        feed.raw(&mut editor, 2, "い");
        editor.voice_commit(1).unwrap();
        feed.send(
            &mut editor,
            VoiceEvent::SessionEnd {
                status: EndStatus::Cancelled,
                last_seq: 2,
            },
        )
        .unwrap();
        assert_eq!(text(&editor), "あ");
        assert!(editor.voice().is_none());
        assert!(editor.can_undo());
    }

    #[test]
    fn completed_keeps_review_open_until_every_segment_is_resolved() {
        let mut editor = editor_with("", 0);
        let mut feed = Feed::new();
        feed.start(&mut editor);
        feed.raw(&mut editor, 1, "あ");
        feed.polished(&mut editor, 1, "亜");
        feed.raw(&mut editor, 2, "い");
        feed.polished(&mut editor, 2, "伊");
        feed.send(
            &mut editor,
            VoiceEvent::SessionEnd {
                status: EndStatus::Completed,
                last_seq: 2,
            },
        )
        .unwrap();
        assert_eq!(pending(&mut editor).len(), 2);
        editor.voice_commit_all().unwrap();
        assert_eq!(text(&editor), "亜伊");
        assert!(editor.voice().is_none());
        // Two history steps, one per segment.
        editor.dispatch(crate::EditorCommand::Undo).unwrap();
        assert_eq!(text(&editor), "亜");
        editor.dispatch(crate::EditorCommand::Undo).unwrap();
        assert_eq!(text(&editor), "");
    }

    #[test]
    fn discarding_everything_leaves_no_history() {
        let mut editor = editor_with("本文", 6);
        let mut feed = Feed::new();
        feed.start(&mut editor);
        feed.raw(&mut editor, 1, "あ");
        feed.raw(&mut editor, 2, "い");
        editor.voice_discard(1).unwrap();
        editor.voice_discard_all().unwrap();
        assert_eq!(text(&editor), "本文");
        assert!(!editor.can_undo());
        assert_eq!(editor.document().revision(), Revision(0));
    }

    #[test]
    fn engine_contract_violations_drop_the_session() {
        let violations: [Vec<VoiceEvent>; 4] = [
            // Raw sequence gap.
            vec![
                VoiceEvent::SessionStart,
                VoiceEvent::SegmentRaw {
                    seq: 2,
                    status: RawStatus::Silent,
                },
            ],
            // Polish without a raw segment.
            vec![
                VoiceEvent::SessionStart,
                VoiceEvent::SegmentPolished {
                    seq: 1,
                    result: PolishResult::Ok("x".into()),
                },
            ],
            // lastSeq mismatch.
            vec![
                VoiceEvent::SessionStart,
                VoiceEvent::SessionEnd {
                    status: EndStatus::Completed,
                    last_seq: 3,
                },
            ],
            // Raw before start.
            vec![VoiceEvent::SegmentRaw {
                seq: 1,
                status: RawStatus::Silent,
            }],
        ];
        for events in violations {
            let mut editor = editor_with("t", 1);
            let mut feed = Feed::new();
            let results: Vec<_> = events
                .into_iter()
                .map(|event| feed.send(&mut editor, event))
                .collect();
            assert!(matches!(
                results.last().unwrap(),
                Err(VoiceError::Contract(_))
            ));
            assert!(editor.voice().is_none());
            assert!(editor.voice_notice().is_some());
            assert_eq!(text(&editor), "t");
        }
    }

    #[test]
    fn duplicates_are_ignored_and_conflicting_reuse_is_a_violation() {
        let mut editor = editor_with("", 0);
        let start = VoiceEnvelope {
            session_id: SESSION.into(),
            event_no: 1,
            event: VoiceEvent::SessionStart,
        };
        assert_eq!(editor.voice_apply(start.clone()), Ok(VoiceApply::Applied));
        assert_eq!(editor.voice_apply(start), Ok(VoiceApply::Duplicate));
        let other = VoiceEnvelope {
            session_id: "ffffffffffffffffffffffffffffffff".into(),
            event_no: 2,
            event: VoiceEvent::SessionStart,
        };
        assert_eq!(
            editor.voice_apply(other),
            Ok(VoiceApply::Ignored(IgnoreReason::WrongSession))
        );
        let conflicting = VoiceEnvelope {
            session_id: SESSION.into(),
            event_no: 1,
            event: VoiceEvent::SegmentRaw {
                seq: 1,
                status: RawStatus::Silent,
            },
        };
        assert!(matches!(
            editor.voice_apply(conflicting),
            Err(VoiceError::Contract(_))
        ));
        assert!(editor.voice().is_none());
    }

    #[test]
    fn events_after_the_end_and_unfinished_polish_are_handled() {
        let mut editor = editor_with("", 0);
        let mut feed = Feed::new();
        feed.start(&mut editor);
        feed.raw(&mut editor, 1, "あ");
        // Completed while a polish is still owed is a violation.
        let result = feed.send(
            &mut editor,
            VoiceEvent::SessionEnd {
                status: EndStatus::Completed,
                last_seq: 1,
            },
        );
        assert!(matches!(result, Err(VoiceError::Contract(_))));
    }

    #[test]
    fn an_ascii_boundary_gets_a_separator_that_belongs_to_the_later_segment() {
        let mut editor = editor_with("", 0);
        let mut feed = Feed::new();
        feed.start(&mut editor);
        feed.raw(&mut editor, 1, "hello");
        feed.raw(&mut editor, 2, "world");
        feed.raw(&mut editor, 3, "世界");
        let shown: Vec<_> = pending(&mut editor).into_iter().map(|s| s.2).collect();
        assert_eq!(shown, ["hello", " world", "世界"]);
        editor.voice_commit(2).unwrap();
        assert_eq!(text(&editor), " world");
        editor.dispatch(crate::EditorCommand::Undo).unwrap();
        assert_eq!(text(&editor), "");
    }

    #[test]
    fn text_limits_are_enforced() {
        let mut editor = editor_with("", 0);
        let mut feed = Feed::new();
        feed.start(&mut editor);
        let result = feed.send(
            &mut editor,
            VoiceEvent::SegmentRaw {
                seq: 1,
                status: RawStatus::Ok {
                    text: "あ".repeat(MAX_SEGMENT_BYTES),
                    polish: PolishMode::Off,
                },
            },
        );
        assert!(matches!(result, Err(VoiceError::Contract(_))));
    }

    #[test]
    fn a_lost_history_drops_the_session() {
        let mut editor = editor_with("", 0);
        let mut feed = Feed::new();
        feed.start(&mut editor);
        feed.raw(&mut editor, 1, "あ");
        for _ in 0..5_000 {
            editor
                .replace_range_recorded(
                    SourceRange::empty(0),
                    "x",
                    Selection::caret(SourceOffset(1)),
                )
                .unwrap();
        }
        editor.voice_sync();
        // Either the deltas still reach back (segment followed) or the session
        // was dropped; the text is never changed by it.
        assert_eq!(text(&editor).len(), 5_000);
        if let Some(voice) = editor.voice() {
            assert_eq!(voice.segments()[0].offset(), 5_000);
        } else {
            assert!(editor.voice_notice().is_some());
        }
    }
}
