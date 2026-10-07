//! Streaming, UI-independent literal search over disk readers and Rope snapshots.

use crate::identity::{FileIdentity, FileStamp};
use crate::service::{FileService, ReadFile};
use crate::session::SessionId;
use grep_matcher::{LineTerminator, Matcher};
use grep_regex::{RegexMatcher, RegexMatcherBuilder};
use grep_searcher::{BinaryDetection, MmapChoice, Searcher, SearcherBuilder, Sink, SinkMatch};
use hane_document::{Revision, RopeSnapshot, SourceRange};
use std::collections::VecDeque;
use std::fmt;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

pub const MAX_SEARCH_QUERY_BYTES: usize = 4 * 1024;
pub const MAX_SEARCH_HITS_PER_DOCUMENT: usize = 1_000;
pub const MAX_SEARCH_HITS_TOTAL: usize = 10_000;
pub const MAX_SEARCH_CONTEXT_BYTES: usize = 1024;
pub const MAX_SEARCHER_HEAP_BYTES: usize = 1024 * 1024;
pub const MAX_SEARCH_READ_BYTES: usize = 64 * 1024;
pub const MAX_SEARCH_RESULT_TEXT_BYTES: usize = 32 * 1024 * 1024;
// Bounds buffered-but-undelivered `FileSearchResult`s. The UI drains this
// channel once per `SEARCH_DELIVERY_POLL` tick (currently 16ms), so sustained
// delivery throughput is capacity / tick-period. 10,000 warm files in <=3s
// needs >=~54 files/tick; 128 keeps a bounded channel (not an unbounded
// queue) while giving headroom over that minimum.
pub const MAX_SEARCH_QUEUED_FILES: usize = 128;
pub const MAX_SEARCH_ROWS_PER_FRAME: usize = 128;
pub const MAX_SEARCH_ERROR_DETAILS: usize = 20;
const MAX_ERROR_DETAIL_BYTES: usize = 512;

/// Identifies one workspace snapshot and one settled query.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct SearchKey {
    pub workspace_epoch: u64,
    pub query_epoch: u64,
}

/// Stable identity of a file or an unsaved draft.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum SearchTarget {
    File(PathBuf),
    Draft(SessionId),
}

/// Content revision used to reject stale hits before navigation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SearchVersion {
    Buffer {
        session: SessionId,
        generation: u64,
        revision: Revision,
    },
    Disk {
        stamp: Option<FileStamp>,
    },
}

/// A validated literal query. Newlines are rejected rather than rewritten.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SearchQuery {
    text: String,
    case_sensitive: bool,
}

/// A matcher construction error without exposing a ripgrep crate type.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SearchEngineBuildError {
    detail: String,
}

impl fmt::Display for SearchEngineBuildError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.detail)
    }
}

impl std::error::Error for SearchEngineBuildError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SearchQueryError {
    Empty,
    ContainsLineBreak,
    TooLong,
}

impl fmt::Display for SearchQueryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str("search text is empty"),
            Self::ContainsLineBreak => formatter.write_str("search text cannot contain CR or LF"),
            Self::TooLong => write!(
                formatter,
                "search text exceeds the {MAX_SEARCH_QUERY_BYTES}-byte limit"
            ),
        }
    }
}

impl std::error::Error for SearchQueryError {}

impl SearchQuery {
    pub fn new(text: impl Into<String>, case_sensitive: bool) -> Result<Self, SearchQueryError> {
        let text = text.into();
        if text.is_empty() {
            return Err(SearchQueryError::Empty);
        }
        if text.contains(['\r', '\n']) {
            return Err(SearchQueryError::ContainsLineBreak);
        }
        if text.len() > MAX_SEARCH_QUERY_BYTES {
            return Err(SearchQueryError::TooLong);
        }
        Ok(Self {
            text,
            case_sensitive,
        })
    }

    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    #[must_use]
    pub fn is_case_sensitive(&self) -> bool {
        self.case_sensitive
    }
}

/// Shared cancellation state checked by the reader and by every match callback.
#[derive(Clone, Debug, Default)]
pub struct SearchCancellationToken(Arc<AtomicBool>);

impl SearchCancellationToken {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }

    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

/// One request shared by a worker while it searches its assigned documents.
#[derive(Clone, Debug)]
pub struct SearchRequest {
    pub key: SearchKey,
    pub query: SearchQuery,
    pub cancellation: SearchCancellationToken,
}

impl SearchRequest {
    #[must_use]
    pub fn new(key: SearchKey, query: SearchQuery, cancellation: SearchCancellationToken) -> Self {
        Self {
            key,
            query,
            cancellation,
        }
    }
}

/// A source selected by the caller at one UI/session snapshot boundary.
pub struct SearchInput {
    target: SearchTarget,
    version: SearchVersion,
    identity: Option<FileIdentity>,
    content: SearchContent,
}

enum SearchContent {
    Disk(ReadFile),
    Buffer(RopeSnapshot),
}

impl SearchInput {
    /// Searches from a reader opened by `FileService::open_reader`.
    #[must_use]
    pub fn disk(path: impl Into<PathBuf>, reader: ReadFile) -> Self {
        let stamp = reader.stamp_at_open();
        let identity = Some(reader.identity().clone());
        Self {
            target: SearchTarget::File(path.into()),
            version: SearchVersion::Disk { stamp },
            identity,
            content: SearchContent::Disk(reader),
        }
    }

    /// Searches an already-open file buffer captured with its session version.
    #[must_use]
    pub fn file_buffer(
        path: impl Into<PathBuf>,
        session: SessionId,
        generation: u64,
        revision: Revision,
        snapshot: RopeSnapshot,
    ) -> Self {
        Self::buffer(
            SearchTarget::File(path.into()),
            session,
            generation,
            revision,
            snapshot,
        )
    }

    /// Searches an existing unsaved draft from its immutable Rope snapshot.
    #[must_use]
    pub fn draft_buffer(
        session: SessionId,
        generation: u64,
        revision: Revision,
        snapshot: RopeSnapshot,
    ) -> Self {
        Self::buffer(
            SearchTarget::Draft(session),
            session,
            generation,
            revision,
            snapshot,
        )
    }

    fn buffer(
        target: SearchTarget,
        session: SessionId,
        generation: u64,
        revision: Revision,
        snapshot: RopeSnapshot,
    ) -> Self {
        Self {
            target,
            version: SearchVersion::Buffer {
                session,
                generation,
                revision,
            },
            identity: None,
            content: SearchContent::Buffer(snapshot),
        }
    }

    fn reader_mut(&mut self) -> &mut dyn Read {
        match &mut self.content {
            SearchContent::Disk(reader) => reader,
            SearchContent::Buffer(snapshot) => snapshot,
        }
    }

    fn disk_stamp_after(&self) -> io::Result<Option<FileStamp>> {
        match &self.content {
            SearchContent::Disk(reader) => reader.current_stamp(),
            SearchContent::Buffer(_) => Ok(None),
        }
    }

    fn is_disk(&self) -> bool {
        matches!(&self.content, SearchContent::Disk(_))
    }
}

/// Source-byte range and a bounded source excerpt for one non-overlapping hit.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SearchHit {
    pub range: SourceRange,
    pub line_number: u64,
    pub context_range: SourceRange,
    pub context_source: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SearchFileCompletion {
    Complete,
    LimitReached,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FileSearchResult {
    pub key: SearchKey,
    pub target: SearchTarget,
    pub version: SearchVersion,
    pub identity: Option<FileIdentity>,
    pub hits: Vec<SearchHit>,
    pub completion: SearchFileCompletion,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SearchWarningKind {
    OpenFailed,
    InvalidUtf8,
    NulByte,
    ReadFailed,
    StampChanged,
    StampUnavailable,
    LineTooLong,
    Internal,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SearchWarning {
    pub key: SearchKey,
    pub target: SearchTarget,
    pub kind: SearchWarningKind,
    pub detail: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SearchCompletion {
    Complete,
    Partial,
    Cancelled,
    Failed,
}

/// Every event crossing into a controller carries the generation that owns it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SearchEvent {
    File {
        key: SearchKey,
        result: FileSearchResult,
    },
    Warning {
        key: SearchKey,
        warning: SearchWarning,
    },
    Progress {
        key: SearchKey,
        files_scanned: u64,
        files_total: u64,
        hits_found: u64,
    },
    Finished {
        key: SearchKey,
        completion: SearchCompletion,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SearchOutcome {
    File(FileSearchResult),
    Warning(SearchWarning),
    Cancelled {
        key: SearchKey,
        target: SearchTarget,
    },
}

/// A worker-local matcher and searcher. Reuse one engine across documents.
pub struct SearchEngine {
    key: SearchKey,
    cancellation: SearchCancellationToken,
    matcher: RegexMatcher,
    searcher: Searcher,
    read_buffer: Box<[u8; MAX_SEARCH_READ_BYTES]>,
}

impl SearchEngine {
    pub fn new(request: SearchRequest) -> Result<Self, SearchEngineBuildError> {
        let mut matcher_builder = RegexMatcherBuilder::new();
        matcher_builder
            .case_insensitive(!request.query.is_case_sensitive())
            .unicode(true)
            .multi_line(false);
        let pattern = escape_literal_query(request.query.text());
        let matcher = matcher_builder
            .build(&pattern)
            .map_err(|error| SearchEngineBuildError {
                detail: bounded_display_detail(&error),
            })?;

        let mut searcher_builder = SearcherBuilder::new();
        searcher_builder
            .line_terminator(LineTerminator::byte(b'\n'))
            .line_number(true)
            .multi_line(false)
            .before_context(0)
            .after_context(0)
            .memory_map(MmapChoice::never())
            .binary_detection(BinaryDetection::none())
            .encoding(None)
            .bom_sniffing(false)
            .heap_limit(Some(MAX_SEARCHER_HEAP_BYTES));

        Ok(Self {
            key: request.key,
            cancellation: request.cancellation,
            matcher,
            searcher: searcher_builder.build(),
            read_buffer: Box::new([0; MAX_SEARCH_READ_BYTES]),
        })
    }

    /// Returns true when the compiled literal matcher accepts the entire
    /// candidate byte slice. Used to re-check a saved match after a document
    /// has been loaded through the normal open path.
    #[must_use]
    pub fn matches_exact(&self, candidate: &[u8]) -> bool {
        self.matcher.find(candidate).is_ok_and(|found| {
            found.is_some_and(|found| found.start() == 0 && found.end() == candidate.len())
        })
    }

    /// Opens and searches one disk file through the streaming `FileService`
    /// boundary. Open failures remain warnings and never become empty results.
    pub fn search_file(&mut self, files: &dyn FileService, path: &Path) -> SearchOutcome {
        match files.open_reader(path) {
            Ok(reader) => self.search(SearchInput::disk(path.to_path_buf(), reader)),
            Err(error) => self.warning(
                SearchTarget::File(path.to_path_buf()),
                SearchWarningKind::OpenFailed,
                Some(bounded_error_detail(&error)),
            ),
        }
    }

    /// Searches one file or buffer snapshot and publishes it only after the
    /// whole file was validated, or after an explicit per-document limit.
    pub fn search(&mut self, mut input: SearchInput) -> SearchOutcome {
        if self.cancellation.is_cancelled() {
            return self.cancelled(input.target);
        }

        let mut sink = SearchSink::new(
            &self.matcher,
            self.cancellation.clone(),
            MAX_SEARCH_HITS_PER_DOCUMENT,
        );
        let (search_result, stream_failure) = {
            let mut reader = SearchStreamReader::new(
                input.reader_mut(),
                &self.cancellation,
                &mut self.read_buffer[..],
            );
            let result = self
                .searcher
                .search_reader(&self.matcher, &mut reader, &mut sink);
            (result, reader.failure)
        };

        if self.cancellation.is_cancelled()
            || sink.cancelled
            || stream_failure == Some(StreamFailure::Cancelled)
        {
            return self.cancelled(input.target);
        }

        if let Some(failure) = stream_failure {
            let (kind, detail) = match failure {
                StreamFailure::InvalidUtf8 => (SearchWarningKind::InvalidUtf8, None),
                StreamFailure::NulByte => (SearchWarningKind::NulByte, None),
                StreamFailure::LineTooLong => (SearchWarningKind::LineTooLong, None),
                StreamFailure::Io => (SearchWarningKind::ReadFailed, None),
                StreamFailure::Cancelled => unreachable!(),
            };
            return self.warning(input.target, kind, detail);
        }

        if let Err(error) = search_result {
            if is_searcher_heap_limit_error(&error) {
                return self.warning(input.target, SearchWarningKind::LineTooLong, None);
            }
            let kind = if sink.internal_failure {
                SearchWarningKind::Internal
            } else {
                SearchWarningKind::ReadFailed
            };
            return self.warning(input.target, kind, Some(bounded_error_detail(&error)));
        }

        if input.is_disk() {
            let before = match input.version {
                SearchVersion::Disk { stamp } => stamp,
                SearchVersion::Buffer { .. } => unreachable!(),
            };
            let after = match input.disk_stamp_after() {
                Ok(Some(stamp)) => stamp,
                Ok(None) => {
                    return self.warning(input.target, SearchWarningKind::StampUnavailable, None);
                }
                Err(error) => {
                    return self.warning(
                        input.target,
                        SearchWarningKind::ReadFailed,
                        Some(bounded_error_detail(&error)),
                    );
                }
            };
            let Some(before) = before else {
                return self.warning(input.target, SearchWarningKind::StampUnavailable, None);
            };
            if before != after {
                return self.warning(input.target, SearchWarningKind::StampChanged, None);
            }
        }

        let completion = if sink.limit_reached {
            SearchFileCompletion::LimitReached
        } else {
            SearchFileCompletion::Complete
        };
        SearchOutcome::File(FileSearchResult {
            key: self.key,
            target: input.target,
            version: input.version,
            identity: input.identity,
            hits: sink.hits,
            completion,
        })
    }

    fn cancelled(&self, target: SearchTarget) -> SearchOutcome {
        SearchOutcome::Cancelled {
            key: self.key,
            target,
        }
    }

    fn warning(
        &self,
        target: SearchTarget,
        kind: SearchWarningKind,
        detail: Option<String>,
    ) -> SearchOutcome {
        SearchOutcome::Warning(SearchWarning {
            key: self.key,
            target,
            kind,
            detail,
        })
    }
}

struct SearchSink<'a> {
    matcher: &'a RegexMatcher,
    cancellation: SearchCancellationToken,
    limit: usize,
    hits: Vec<SearchHit>,
    limit_reached: bool,
    cancelled: bool,
    internal_failure: bool,
}

impl<'a> SearchSink<'a> {
    fn new(matcher: &'a RegexMatcher, cancellation: SearchCancellationToken, limit: usize) -> Self {
        Self {
            matcher,
            cancellation,
            limit,
            hits: Vec::new(),
            limit_reached: false,
            cancelled: false,
            internal_failure: false,
        }
    }
}

impl Sink for SearchSink<'_> {
    type Error = io::Error;

    fn matched(
        &mut self,
        _searcher: &Searcher,
        matched: &SinkMatch<'_>,
    ) -> Result<bool, Self::Error> {
        let bytes = matched.bytes();
        let line = std::str::from_utf8(bytes).map_err(|error| {
            self.internal_failure = true;
            io::Error::new(io::ErrorKind::InvalidData, error)
        })?;
        let line_offset = usize::try_from(matched.absolute_byte_offset()).map_err(|error| {
            self.internal_failure = true;
            io::Error::new(io::ErrorKind::InvalidData, error)
        })?;
        let line_number = matched.line_number().unwrap_or(1);
        let content_len = line_content_len(bytes);

        let matcher = self.matcher;
        let cancellation = self.cancellation.clone();
        let limit = self.limit;
        let hits = &mut self.hits;
        let limit_reached = &mut self.limit_reached;
        let cancelled = &mut self.cancelled;
        let mut internal_failure = false;
        let found = matcher.find_iter(bytes, |found| {
            if cancellation.is_cancelled() {
                *cancelled = true;
                return false;
            }
            if hits.len() >= limit {
                *limit_reached = true;
                return false;
            }
            let start = found.start();
            let end = found.end();
            if end > content_len || start > end {
                internal_failure = true;
                return false;
            }
            match make_hit(line, line_offset, line_number, start, end) {
                Some(hit) => hits.push(hit),
                None => internal_failure = true,
            }
            !internal_failure
        });
        if let Err(error) = found {
            self.internal_failure = true;
            return Err(io::Error::other(error));
        }
        if internal_failure {
            self.internal_failure = true;
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "matcher returned a non-source-aligned range",
            ));
        }
        Ok(!self.cancelled && !self.limit_reached)
    }
}

fn line_content_len(bytes: &[u8]) -> usize {
    let mut content_len = bytes.len();
    if bytes.last() == Some(&b'\n') {
        content_len -= 1;
        if content_len > 0 && bytes[content_len - 1] == b'\r' {
            content_len -= 1;
        }
    }
    content_len
}

fn escape_literal_query(query: &str) -> String {
    let mut escaped = String::with_capacity(query.len());
    for character in query.chars() {
        if matches!(
            character,
            '\\' | '.' | '+' | '*' | '?' | '(' | ')' | '[' | ']' | '{' | '}' | '^' | '$' | '|'
        ) {
            escaped.push('\\');
        }
        escaped.push(character);
    }
    escaped
}

fn is_searcher_heap_limit_error(error: &io::Error) -> bool {
    error
        .to_string()
        .starts_with("configured allocation limit (")
}

fn make_hit(
    line: &str,
    line_offset: usize,
    line_number: u64,
    match_start: usize,
    match_end: usize,
) -> Option<SearchHit> {
    if !line.is_char_boundary(match_start) || !line.is_char_boundary(match_end) {
        return None;
    }
    let content_len = line_content_len(line.as_bytes());
    if match_end > content_len {
        return None;
    }

    let source = &line[..content_len];
    let available = MAX_SEARCH_CONTEXT_BYTES.min(source.len());
    let match_len = match_end - match_start;
    let mut context_start = if source.len() <= MAX_SEARCH_CONTEXT_BYTES {
        0
    } else if match_len >= MAX_SEARCH_CONTEXT_BYTES {
        match_start
    } else {
        match_start.saturating_sub((MAX_SEARCH_CONTEXT_BYTES - match_len) / 2)
    };
    while context_start > 0 && !source.is_char_boundary(context_start) {
        context_start -= 1;
    }
    let mut context_end = context_start.saturating_add(available).min(source.len());
    while context_end > context_start && !source.is_char_boundary(context_end) {
        context_end -= 1;
    }
    if match_end <= source.len() && context_end < match_end {
        context_end = match_end;
        while context_end > context_start + MAX_SEARCH_CONTEXT_BYTES
            || !source.is_char_boundary(context_end)
        {
            context_end -= 1;
        }
    }
    if context_end - context_start > MAX_SEARCH_CONTEXT_BYTES {
        return None;
    }

    Some(SearchHit {
        range: SourceRange::new(line_offset + match_start, line_offset + match_end),
        line_number,
        context_range: SourceRange::new(line_offset + context_start, line_offset + context_end),
        context_source: source[context_start..context_end].to_owned(),
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum StreamFailure {
    Cancelled,
    InvalidUtf8,
    NulByte,
    LineTooLong,
    Io,
}

#[derive(Debug)]
struct StreamFailureMarker(StreamFailure);

impl fmt::Display for StreamFailureMarker {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "search stream failure: {:?}", self.0)
    }
}

impl std::error::Error for StreamFailureMarker {}

/// Validates source bytes while preserving every byte offset. Bare CR is
/// converted to LF one-for-one; CRLF is copied unchanged, even across reads.
struct SearchStreamReader<'a> {
    source: &'a mut dyn Read,
    cancellation: &'a SearchCancellationToken,
    raw: &'a mut [u8],
    raw_pos: usize,
    raw_len: usize,
    pending_raw: Option<u8>,
    pending_cr: bool,
    normalized: VecDeque<u8>,
    source_eof: bool,
    eof_validated: bool,
    current_line_bytes: usize,
    utf8: Utf8Validator,
    failure: Option<StreamFailure>,
}

impl<'a> SearchStreamReader<'a> {
    fn new(
        source: &'a mut dyn Read,
        cancellation: &'a SearchCancellationToken,
        raw: &'a mut [u8],
    ) -> Self {
        Self {
            source,
            cancellation,
            raw,
            raw_pos: 0,
            raw_len: 0,
            pending_raw: None,
            pending_cr: false,
            normalized: VecDeque::with_capacity(2),
            source_eof: false,
            eof_validated: false,
            current_line_bytes: 0,
            utf8: Utf8Validator::default(),
            failure: None,
        }
    }

    fn fail(&mut self, failure: StreamFailure) -> io::Error {
        self.failure = Some(failure);
        io::Error::other(StreamFailureMarker(failure))
    }

    fn check_cancelled(&mut self) -> io::Result<()> {
        if self.cancellation.is_cancelled() {
            return Err(self.fail(StreamFailure::Cancelled));
        }
        Ok(())
    }

    fn next_raw_byte(&mut self) -> io::Result<Option<u8>> {
        if let Some(byte) = self.pending_raw.take() {
            return Ok(Some(byte));
        }
        if self.raw_pos < self.raw_len {
            let byte = self.raw[self.raw_pos];
            self.raw_pos += 1;
            return Ok(Some(byte));
        }
        if self.source_eof {
            return Ok(None);
        }

        loop {
            self.check_cancelled()?;
            match self.source.read(self.raw) {
                Ok(read) => {
                    self.check_cancelled()?;
                    self.raw_pos = 0;
                    self.raw_len = read;
                    if read == 0 {
                        self.source_eof = true;
                        return Ok(None);
                    }
                    let byte = self.raw[0];
                    self.raw_pos = 1;
                    return Ok(Some(byte));
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {
                    self.check_cancelled()?;
                }
                Err(error) => {
                    self.failure = Some(StreamFailure::Io);
                    return Err(error);
                }
            }
        }
    }

    fn validate_byte(&mut self, byte: u8) -> io::Result<()> {
        if byte == 0 {
            return Err(self.fail(StreamFailure::NulByte));
        }
        if !self.utf8.push(byte) {
            return Err(self.fail(StreamFailure::InvalidUtf8));
        }
        Ok(())
    }

    fn validate_eof(&mut self) -> io::Result<()> {
        if !self.eof_validated {
            self.eof_validated = true;
            if !self.utf8.is_complete() {
                return Err(self.fail(StreamFailure::InvalidUtf8));
            }
        }
        Ok(())
    }

    fn enqueue_next(&mut self) -> io::Result<bool> {
        if self.pending_cr {
            match self.next_raw_byte()? {
                Some(b'\n') => {
                    self.validate_byte(b'\n')?;
                    self.normalized.push_back(b'\r');
                    self.normalized.push_back(b'\n');
                }
                Some(byte) => {
                    self.pending_raw = Some(byte);
                    self.normalized.push_back(b'\n');
                }
                None => {
                    self.validate_eof()?;
                    self.normalized.push_back(b'\n');
                }
            }
            self.pending_cr = false;
            return Ok(true);
        }

        match self.next_raw_byte()? {
            Some(byte) => {
                self.validate_byte(byte)?;
                if byte == b'\r' {
                    self.pending_cr = true;
                } else {
                    self.normalized.push_back(byte);
                }
                Ok(true)
            }
            None => {
                self.validate_eof()?;
                Ok(false)
            }
        }
    }
}

impl Read for SearchStreamReader<'_> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if output.is_empty() {
            return Ok(0);
        }
        self.check_cancelled()?;
        let mut written = 0;
        loop {
            self.check_cancelled()?;
            if let Some(&byte) = self.normalized.front() {
                if self.current_line_bytes >= MAX_SEARCHER_HEAP_BYTES {
                    if written == 0 {
                        return Err(self.fail(StreamFailure::LineTooLong));
                    }
                    return Ok(written);
                }
                self.normalized.pop_front();
                output[written] = byte;
                written += 1;
                if byte == b'\n' {
                    self.current_line_bytes = 0;
                    return Ok(written);
                }
                self.current_line_bytes += 1;
                if written == output.len() {
                    return Ok(written);
                }
                continue;
            }
            if !self.enqueue_next()? {
                return Ok(written);
            }
        }
    }
}

#[derive(Default)]
struct Utf8Validator {
    remaining: u8,
    next_min: u8,
    next_max: u8,
}

impl Utf8Validator {
    fn push(&mut self, byte: u8) -> bool {
        if self.remaining > 0 {
            if byte < self.next_min || byte > self.next_max {
                return false;
            }
            self.remaining -= 1;
            self.next_min = 0x80;
            self.next_max = 0xBF;
            return true;
        }

        match byte {
            0x00..=0x7F => true,
            0xC2..=0xDF => self.start(1, 0x80, 0xBF),
            0xE0 => self.start(2, 0xA0, 0xBF),
            0xE1..=0xEC | 0xEE..=0xEF => self.start(2, 0x80, 0xBF),
            0xED => self.start(2, 0x80, 0x9F),
            0xF0 => self.start(3, 0x90, 0xBF),
            0xF1..=0xF3 => self.start(3, 0x80, 0xBF),
            0xF4 => self.start(3, 0x80, 0x8F),
            _ => false,
        }
    }

    fn start(&mut self, remaining: u8, next_min: u8, next_max: u8) -> bool {
        self.remaining = remaining;
        self.next_min = next_min;
        self.next_max = next_max;
        true
    }

    fn is_complete(&self) -> bool {
        self.remaining == 0
    }
}

fn bounded_error_detail(error: &io::Error) -> String {
    bounded_display_detail(error)
}

fn bounded_display_detail(error: &impl fmt::Display) -> String {
    let detail = error.to_string();
    let mut end = detail.len().min(MAX_ERROR_DETAIL_BYTES);
    while !detail.is_char_boundary(end) {
        end -= 1;
    }
    detail[..end].to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::FileService;
    use crate::testing::{MemoryFileService, MemoryReadBehavior};
    use hane_document::{RopeBuffer, TextBuffer};
    use std::io::Cursor;
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
    use std::time::Duration;

    fn key() -> SearchKey {
        SearchKey {
            workspace_epoch: 7,
            query_epoch: 11,
        }
    }

    fn engine(query: &str, case_sensitive: bool) -> (SearchEngine, SearchCancellationToken) {
        let query = SearchQuery::new(query, case_sensitive).unwrap();
        let cancellation = SearchCancellationToken::new();
        let engine =
            SearchEngine::new(SearchRequest::new(key(), query, cancellation.clone())).unwrap();
        (engine, cancellation)
    }

    fn search_text(engine: &mut SearchEngine, text: &str) -> SearchOutcome {
        let buffer = RopeBuffer::from_text(text);
        let input = SearchInput::file_buffer(
            "/work/note.md",
            SessionId(4),
            2,
            buffer.revision(),
            buffer.snapshot(),
        );
        engine.search(input)
    }

    fn disk_input(service: &MemoryFileService, path: &str) -> SearchInput {
        SearchInput::disk(
            path,
            service.open_reader(std::path::Path::new(path)).unwrap(),
        )
    }

    #[test]
    fn literal_search_reports_non_overlapping_utf8_byte_ranges() {
        let (mut engine, _) = engine("日本語", true);
        let result = search_text(&mut engine, "x日本語🙂日本語\n日本語x");
        let SearchOutcome::File(result) = result else {
            panic!("expected a completed file result");
        };
        assert_eq!(
            result.hits.iter().map(|hit| hit.range).collect::<Vec<_>>(),
            [
                SourceRange::new(1, 10),
                SourceRange::new(14, 23),
                SourceRange::new(24, 33)
            ]
        );
        assert_eq!(
            result
                .hits
                .iter()
                .map(|hit| hit.line_number)
                .collect::<Vec<_>>(),
            [1, 1, 2]
        );
    }

    #[test]
    fn overlapping_candidates_are_reported_once_from_their_non_overlapping_starts() {
        let (mut engine, _) = engine("aba", true);
        let SearchOutcome::File(result) = search_text(&mut engine, "ababa") else {
            panic!("expected a completed file result");
        };
        assert_eq!(result.hits.len(), 1);
        assert_eq!(result.hits[0].range, SourceRange::new(0, 3));
    }

    #[test]
    fn literal_search_does_not_interpret_regex_syntax() {
        let (mut engine, _) = engine("a.*[]", true);
        let result = search_text(&mut engine, "a.*[] aZZ[]");
        let SearchOutcome::File(result) = result else {
            panic!("expected a completed file result");
        };
        assert_eq!(result.hits.len(), 1);
        assert_eq!(result.hits[0].range, SourceRange::new(0, 5));
    }

    #[test]
    fn case_sensitive_search_can_be_disabled() {
        let (mut exact, _) = engine("note", true);
        let exact = search_text(&mut exact, "Note note NOTE");
        let SearchOutcome::File(exact) = exact else {
            panic!("expected a completed file result");
        };
        assert_eq!(exact.hits.len(), 1);
        assert_eq!(exact.hits[0].range, SourceRange::new(5, 9));

        let (mut insensitive, _) = engine("note", false);
        let insensitive = search_text(&mut insensitive, "Note note NOTE");
        let SearchOutcome::File(insensitive) = insensitive else {
            panic!("expected a completed file result");
        };
        assert_eq!(insensitive.hits.len(), 3);
    }

    #[test]
    fn compiled_literal_matcher_verifies_the_entire_candidate() {
        let (engine, _) = engine("note", false);
        assert!(engine.matches_exact(b"NOTE"));
        assert!(!engine.matches_exact(b"prefix note"));
        assert!(!engine.matches_exact(b"notebook"));
    }

    #[test]
    fn query_validation_keeps_whitespace_and_rejects_newlines_and_oversize() {
        assert_eq!(SearchQuery::new("  ", true).unwrap().text(), "  ");
        assert_eq!(
            SearchQuery::new("a\rb", true),
            Err(SearchQueryError::ContainsLineBreak)
        );
        assert_eq!(
            SearchQuery::new("a\nb", true),
            Err(SearchQueryError::ContainsLineBreak)
        );
        assert_eq!(
            SearchQuery::new("x".repeat(MAX_SEARCH_QUERY_BYTES + 1), true),
            Err(SearchQueryError::TooLong)
        );
    }

    #[test]
    fn disk_search_does_not_load_or_save_a_document() {
        let service = MemoryFileService::new();
        service.write_externally("/work/a.md", "needle\n");
        let (mut engine, _) = engine("needle", true);
        let outcome = engine.search(disk_input(&service, "/work/a.md"));
        assert!(matches!(outcome, SearchOutcome::File(_)));
        assert_eq!(service.load_calls(), 0);
        assert_eq!(service.save_calls(), 0);
    }

    #[test]
    fn a_missing_disk_file_is_a_warning_instead_of_an_empty_result() {
        let service = MemoryFileService::new();
        let (mut engine, _) = engine("needle", true);
        let outcome = engine.search_file(&service, Path::new("/work/missing.md"));
        assert!(matches!(
            outcome,
            SearchOutcome::Warning(SearchWarning {
                kind: SearchWarningKind::OpenFailed,
                ..
            })
        ));
    }

    #[test]
    fn search_prefers_an_existing_buffer_snapshot_over_disk() {
        let service = MemoryFileService::new();
        service.write_externally("/work/a.md", "disk only\n");
        let buffer = RopeBuffer::from_text("unsaved needle\n");
        let input = SearchInput::file_buffer(
            "/work/a.md",
            SessionId(9),
            3,
            buffer.revision(),
            buffer.snapshot(),
        );
        let (mut engine, _) = engine("needle", true);
        let SearchOutcome::File(result) = engine.search(input) else {
            panic!("expected a completed file result");
        };
        assert_eq!(result.hits.len(), 1);
        assert!(matches!(
            result.version,
            SearchVersion::Buffer {
                session: SessionId(9),
                generation: 3,
                ..
            }
        ));
        assert_eq!(service.load_calls(), 0);
    }

    #[test]
    fn draft_search_uses_its_existing_session_snapshot() {
        let buffer = RopeBuffer::from_text("draft needle");
        let input =
            SearchInput::draft_buffer(SessionId(10), 1, buffer.revision(), buffer.snapshot());
        let (mut engine, _) = engine("needle", true);
        let SearchOutcome::File(result) = engine.search(input) else {
            panic!("expected a completed draft result");
        };
        assert_eq!(result.target, SearchTarget::Draft(SessionId(10)));
        assert_eq!(result.hits[0].range, SourceRange::new(6, 12));
    }

    #[test]
    fn line_endings_preserve_source_byte_ranges_and_line_numbers() {
        let service = MemoryFileService::new();
        service.write_externally("/work/a.md", "one\r\ntwo\rthree\n終端なし needle");
        service.set_read_behavior(
            "/work/a.md",
            MemoryReadBehavior {
                max_chunk_bytes: Some(1),
                ..MemoryReadBehavior::default()
            },
        );
        let (mut engine, _) = engine("needle", true);
        let SearchOutcome::File(result) = engine.search(disk_input(&service, "/work/a.md")) else {
            panic!("expected a completed file result");
        };
        let start = "one\r\ntwo\rthree\n終端なし ".len();
        assert_eq!(
            result.hits[0].range,
            SourceRange::new(start, start + "needle".len())
        );
        assert_eq!(result.hits[0].line_number, 4);
    }

    #[test]
    fn crlf_split_across_reads_is_one_line_and_bare_cr_is_a_line_break() {
        let service = MemoryFileService::new();
        service.write_externally("/work/a.md", "a\r\nb\rX\nc");
        service.set_read_behavior(
            "/work/a.md",
            MemoryReadBehavior {
                max_chunk_bytes: Some(1),
                ..MemoryReadBehavior::default()
            },
        );
        let (mut engine, _) = engine("X", true);
        let SearchOutcome::File(result) = engine.search(disk_input(&service, "/work/a.md")) else {
            panic!("expected a completed file result");
        };
        assert_eq!(result.hits[0].range, SourceRange::new(5, 6));
        assert_eq!(result.hits[0].line_number, 3);
    }

    #[test]
    fn bom_bytes_are_included_in_source_ranges() {
        let (mut engine, _) = engine("needle", true);
        let result = search_text(&mut engine, "\u{feff}needle\nneedle");
        let SearchOutcome::File(result) = result else {
            panic!("expected a completed file result");
        };
        assert_eq!(
            result.hits.iter().map(|hit| hit.range).collect::<Vec<_>>(),
            [SourceRange::new(3, 9), SourceRange::new(10, 16)]
        );
    }

    #[test]
    fn cancellation_is_observed_while_scanning_a_large_zero_match_file() {
        let service = MemoryFileService::new();
        service.write_externally("/work/large.md", &"x".repeat(MAX_SEARCH_READ_BYTES * 8));
        let cancellation = SearchCancellationToken::new();
        let query = SearchQuery::new("missing", true).unwrap();
        let mut engine =
            SearchEngine::new(SearchRequest::new(key(), query, cancellation.clone())).unwrap();
        service.set_read_behavior(
            "/work/large.md",
            MemoryReadBehavior {
                max_chunk_bytes: Some(MAX_SEARCH_READ_BYTES),
                delay_per_read: Some(Duration::from_millis(1)),
                ..MemoryReadBehavior::default()
            },
        );
        let input = disk_input(&service, "/work/large.md");
        let canceller = cancellation.clone();
        let cancel_thread = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(5));
            canceller.cancel();
        });
        let outcome = engine.search(input);
        cancel_thread.join().unwrap();
        assert!(matches!(outcome, SearchOutcome::Cancelled { .. }));
    }

    #[test]
    fn already_cancelled_requests_do_not_read_the_source() {
        let service = MemoryFileService::new();
        service.write_externally("/work/a.md", "needle");
        let cancellation = SearchCancellationToken::new();
        cancellation.cancel();
        let query = SearchQuery::new("needle", true).unwrap();
        let mut engine = SearchEngine::new(SearchRequest::new(key(), query, cancellation)).unwrap();
        let outcome = engine.search(disk_input(&service, "/work/a.md"));
        assert!(matches!(outcome, SearchOutcome::Cancelled { .. }));
    }

    #[test]
    fn per_document_limit_is_reported_and_does_not_claim_completion() {
        let source = "x ".repeat(MAX_SEARCH_HITS_PER_DOCUMENT + 2);
        let (mut engine, _) = engine("x", true);
        let SearchOutcome::File(result) = search_text(&mut engine, &source) else {
            panic!("expected a limited file result");
        };
        assert_eq!(result.hits.len(), MAX_SEARCH_HITS_PER_DOCUMENT);
        assert_eq!(result.completion, SearchFileCompletion::LimitReached);
    }

    #[test]
    fn invalid_utf8_discards_earlier_matches_in_the_same_file() {
        let service = MemoryFileService::new();
        service.write_bytes_externally("/work/a.md", b"needle\ninvalid \xff");
        let (mut engine, _) = engine("needle", true);
        let outcome = engine.search(disk_input(&service, "/work/a.md"));
        assert!(matches!(
            outcome,
            SearchOutcome::Warning(SearchWarning {
                kind: SearchWarningKind::InvalidUtf8,
                ..
            })
        ));
    }

    #[test]
    fn a_truncated_utf8_scalar_at_eof_is_a_warning() {
        let service = MemoryFileService::new();
        service.write_bytes_externally("/work/a.md", b"needle\ntruncated \xe2\x82");
        let (mut engine, _) = engine("needle", true);
        let outcome = engine.search(disk_input(&service, "/work/a.md"));
        assert!(matches!(
            outcome,
            SearchOutcome::Warning(SearchWarning {
                kind: SearchWarningKind::InvalidUtf8,
                ..
            })
        ));
    }

    #[test]
    fn nul_byte_is_reported_as_a_binary_warning() {
        let service = MemoryFileService::new();
        service.write_externally("/work/a.md", "needle\0 later");
        let (mut engine, _) = engine("needle", true);
        let outcome = engine.search(disk_input(&service, "/work/a.md"));
        assert!(matches!(
            outcome,
            SearchOutcome::Warning(SearchWarning {
                kind: SearchWarningKind::NulByte,
                ..
            })
        ));
    }

    #[test]
    fn read_failure_discards_earlier_matches_in_the_same_file() {
        let service = MemoryFileService::new();
        service.write_externally("/work/a.md", "needle\nrest");
        service.set_read_behavior(
            "/work/a.md",
            MemoryReadBehavior {
                max_chunk_bytes: Some(7),
                fail_after_bytes: Some(7),
                ..MemoryReadBehavior::default()
            },
        );
        let (mut engine, _) = engine("needle", true);
        let outcome = engine.search(disk_input(&service, "/work/a.md"));
        assert!(matches!(
            outcome,
            SearchOutcome::Warning(SearchWarning {
                kind: SearchWarningKind::ReadFailed,
                ..
            })
        ));
    }

    #[test]
    fn changed_reader_stamp_discards_the_results() {
        let service = MemoryFileService::new();
        service.write_externally("/work/a.md", "needle\nrest");
        service.set_read_behavior(
            "/work/a.md",
            MemoryReadBehavior {
                change_stamp_after_bytes: Some(4),
                ..MemoryReadBehavior::default()
            },
        );
        let (mut engine, _) = engine("needle", true);
        let outcome = engine.search(disk_input(&service, "/work/a.md"));
        assert!(matches!(
            outcome,
            SearchOutcome::Warning(SearchWarning {
                kind: SearchWarningKind::StampChanged,
                ..
            })
        ));
    }

    #[test]
    fn a_line_over_the_searcher_heap_limit_is_not_reported_as_complete() {
        let service = MemoryFileService::new();
        let mut source = "x".repeat(MAX_SEARCHER_HEAP_BYTES + 8);
        source.push_str(" needle");
        service.write_externally("/work/a.md", &source);
        let (mut engine, _) = engine("needle", true);
        let outcome = engine.search(disk_input(&service, "/work/a.md"));
        assert!(matches!(
            outcome,
            SearchOutcome::Warning(SearchWarning {
                kind: SearchWarningKind::LineTooLong,
                ..
            })
        ));
    }

    #[test]
    fn context_excerpt_is_bounded_and_uses_utf8_boundaries() {
        let source = format!("{}needle{}", "あ".repeat(500), "🙂".repeat(500));
        let (mut engine, _) = engine("needle", true);
        let SearchOutcome::File(result) = search_text(&mut engine, &source) else {
            panic!("expected a completed file result");
        };
        let hit = &result.hits[0];
        assert!(hit.context_source.len() <= MAX_SEARCH_CONTEXT_BYTES);
        assert_eq!(hit.context_range.len_bytes(), hit.context_source.len());
        assert!(
            hit.context_source
                .is_char_boundary(hit.context_source.len())
        );
        assert!(hit.range.start.0 >= hit.context_range.start.0);
        assert!(hit.range.end.0 <= hit.context_range.end.0);
    }

    #[test]
    fn memory_reader_can_split_utf8_at_every_byte_boundary() {
        let service = MemoryFileService::new();
        service.write_externally("/work/a.md", "🙂needle日本語");
        service.set_read_behavior(
            "/work/a.md",
            MemoryReadBehavior {
                max_chunk_bytes: Some(1),
                ..MemoryReadBehavior::default()
            },
        );
        let (mut engine, _) = engine("needle", true);
        let SearchOutcome::File(result) = engine.search(disk_input(&service, "/work/a.md")) else {
            panic!("expected a completed file result");
        };
        assert_eq!(result.hits[0].range, SourceRange::new(4, 10));
    }

    #[test]
    fn file_result_carries_the_request_key_and_disk_stamp() {
        let service = MemoryFileService::new();
        service.write_externally("/work/a.md", "needle");
        let (mut engine, _) = engine("needle", true);
        let SearchOutcome::File(result) = engine.search(disk_input(&service, "/work/a.md")) else {
            panic!("expected a completed file result");
        };
        assert_eq!(result.key, key());
        assert!(
            result
                .identity
                .as_ref()
                .is_some_and(|identity| identity.path() == Path::new("/work/a.md"))
        );
        assert!(matches!(
            result.version,
            SearchVersion::Disk { stamp: Some(_) }
        ));
    }

    struct CancelOnRead {
        bytes: Cursor<Vec<u8>>,
        token: SearchCancellationToken,
        calls: Arc<AtomicUsize>,
    }

    impl Read for CancelOnRead {
        fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
            let read = self.bytes.read(output)?;
            self.calls.fetch_add(1, AtomicOrdering::Relaxed);
            self.token.cancel();
            Ok(read)
        }
    }

    impl crate::service::StampedRead for CancelOnRead {
        fn current_stamp(&self) -> io::Result<Option<FileStamp>> {
            Ok(Some(FileStamp::new(8, None)))
        }
    }

    #[test]
    fn cancellation_after_a_read_is_observed_before_any_hit_is_published() {
        let cancellation = SearchCancellationToken::new();
        let calls = Arc::new(AtomicUsize::new(0));
        let reader = ReadFile::from_reader(
            CancelOnRead {
                bytes: Cursor::new(b"no match\n".to_vec()),
                token: cancellation.clone(),
                calls: Arc::clone(&calls),
            },
            FileIdentity::lexical("/work/a.md"),
        )
        .unwrap();
        // The token is flipped by the source read itself, covering the
        // post-read cancellation check without relying on scheduler timing.
        let query = SearchQuery::new("needle", true).unwrap();
        let mut engine =
            SearchEngine::new(SearchRequest::new(key(), query, cancellation.clone())).unwrap();
        let outcome = engine.search(SearchInput::disk("/work/a.md", reader));
        assert!(matches!(outcome, SearchOutcome::Cancelled { .. }));
        assert_eq!(calls.load(AtomicOrdering::Relaxed), 1);
    }
}
