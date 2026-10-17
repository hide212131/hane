//! Test-only support: deterministic signing key, fake transport / clock /
//! filesystem seams and ZIP fixture builders.
//!
//! The signing seed below is a public test constant. It is not, and must never
//! be copied into, the production trust policy.

use std::collections::{HashMap, VecDeque};
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use ring::signature::{Ed25519KeyPair, KeyPair};

use crate::clock::Clock;
use crate::download::{
    BodyReader, PartStore, Request, Response, Transport, TransportError, TransportFailure,
};
use crate::error::{ErrorCode, Result, UpdateError};
use crate::identity::{FileIdentity, Inventory, InventoryFile};
use crate::journal::{DiskView, JournalStore, Observed, UpdateJournal};
use crate::model::{
    CATALOG_DOMAIN, CATALOG_SCHEMA, CHANNEL_STABLE, CatalogDocument, INVENTORY_DOMAIN,
    INVENTORY_FILE_NAME, INVENTORY_SCHEMA, INVENTORY_SIGNATURE_FILE_NAME, MACOS_BUNDLE_ID,
    REPOSITORY, STATE_COMPATIBILITY_V1, SignatureEntry, SignatureFile, Target, TargetEntry,
    UPDATER_PROTOCOL,
};
use crate::trust::{SeenCatalog, SeenStore, TrustPolicy, TrustedKey};
use crate::util::{parse_rfc3339_utc, sha256_hex};

pub const TEST_KEY_ID: &str = "test-key-1";
pub const TEST_TEAM_ID: &str = "TESTTEAM01";
pub const TEST_SOURCE_SHA: &str = "0123456789abcdef0123456789abcdef01234567";

fn key_pair(seed: u8) -> Ed25519KeyPair {
    Ed25519KeyPair::from_seed_unchecked(&[seed; 32]).expect("test seed is valid")
}

pub fn public_key(seed: u8) -> [u8; 32] {
    let mut out = [0u8; 32];
    out.copy_from_slice(key_pair(seed).public_key().as_ref());
    out
}

/// Policy trusting only the test key (seed byte 1) and a test Team ID.
pub fn test_policy() -> TrustPolicy {
    TrustPolicy::new(
        vec![TrustedKey {
            key_id: TEST_KEY_ID.to_string(),
            public_key: public_key(1),
        }],
        Some(TEST_TEAM_ID.to_string()),
    )
}

/// Signature file over `domain || payload` made with `seed` under `key_id`.
pub fn sign_with(seed: u8, key_id: &str, domain: &[u8], payload: &[u8]) -> Vec<u8> {
    let mut message = domain.to_vec();
    message.extend_from_slice(payload);
    let signature = key_pair(seed).sign(&message);
    let file = SignatureFile {
        schema: 1,
        signatures: vec![SignatureEntry {
            key_id: key_id.to_string(),
            signature_base64: STANDARD.encode(signature.as_ref()),
        }],
    };
    serde_json::to_vec(&file).expect("serialize signature file")
}

pub fn sign(domain: &[u8], payload: &[u8]) -> Vec<u8> {
    sign_with(1, TEST_KEY_ID, domain, payload)
}

pub fn test_now() -> i64 {
    parse_rfc3339_utc("2026-10-17T00:00:00Z").expect("fixed date")
}

pub struct FixedClock {
    now: AtomicI64,
    monotonic: AtomicU64,
    step_ms: u64,
}

impl FixedClock {
    pub fn new(now: i64) -> Self {
        Self::with_step(now, 0)
    }

    /// `monotonic_ms` advances by `step_ms` on every call.
    pub fn with_step(now: i64, step_ms: u64) -> Self {
        Self {
            now: AtomicI64::new(now),
            monotonic: AtomicU64::new(0),
            step_ms,
        }
    }

    /// Moves the monotonic clock forward, e.g. from inside a scripted
    /// transport call to model time spent connecting or redirecting.
    pub fn advance(&self, ms: u64) {
        self.monotonic.fetch_add(ms, Ordering::SeqCst);
    }

    pub fn set_now(&self, now: i64) {
        self.now.store(now, Ordering::SeqCst);
    }
}

impl Clock for FixedClock {
    fn now_unix(&self) -> i64 {
        self.now.load(Ordering::SeqCst)
    }

    fn monotonic_ms(&self) -> u64 {
        self.monotonic.fetch_add(self.step_ms, Ordering::SeqCst)
    }
}

pub struct TempDir {
    path: PathBuf,
}

impl TempDir {
    pub fn new(label: &str) -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        let path = std::env::temp_dir().join(format!(
            "hane-update-{label}-{}-{n}-{nanos}",
            std::process::id()
        ));
        fs::create_dir_all(&path).expect("create temp dir");
        Self { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

// ---------------------------------------------------------------- transport

/// Hands out a body in small chunks, then optionally fails.
pub struct ChunkReader {
    data: Vec<u8>,
    pos: usize,
    then: Option<io::ErrorKind>,
}

impl ChunkReader {
    pub fn new(data: Vec<u8>, then: Option<io::ErrorKind>) -> Self {
        Self { data, pos: 0, then }
    }
}

impl BodyReader for ChunkReader {
    fn read_within(&mut self, buf: &mut [u8], _timeout_ms: u64) -> io::Result<usize> {
        self.read(buf)
    }
}

/// Body whose reads each take `read_ms` of monotonic time. A read that cannot
/// finish within its timeout spends only the timeout and fails with `TimedOut`.
/// With `ignore_timeout` it models a broken body that overruns and still
/// succeeds.
pub struct SlowBody {
    inner: ChunkReader,
    clock: Arc<FixedClock>,
    read_ms: u64,
    ignore_timeout: bool,
}

impl SlowBody {
    pub fn new(data: Vec<u8>, clock: &Arc<FixedClock>, read_ms: u64, ignore_timeout: bool) -> Self {
        Self {
            inner: ChunkReader::new(data, None),
            clock: Arc::clone(clock),
            read_ms,
            ignore_timeout,
        }
    }
}

impl BodyReader for SlowBody {
    fn read_within(&mut self, buf: &mut [u8], timeout_ms: u64) -> io::Result<usize> {
        if !self.ignore_timeout && self.read_ms > timeout_ms {
            self.clock.advance(timeout_ms);
            return Err(io::Error::new(io::ErrorKind::TimedOut, "read timed out"));
        }
        self.clock.advance(self.read_ms);
        self.inner.read(buf)
    }
}

impl Read for ChunkReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.pos < self.data.len() {
            let n = buf.len().min(self.data.len() - self.pos).min(7);
            buf[..n].copy_from_slice(&self.data[self.pos..self.pos + n]);
            self.pos += n;
            return Ok(n);
        }
        match self.then {
            Some(kind) => Err(io::Error::new(kind, "scripted failure")),
            None => Ok(0),
        }
    }
}

pub enum Scripted {
    Respond(Response),
    Fail(TransportError),
    Custom(Box<dyn FnOnce() -> std::result::Result<Response, TransportError> + Send>),
    /// Connecting and receiving the head takes `ms`. If that exceeds the
    /// request's `deadline_ms`, the call spends only the deadline and fails
    /// with a timeout; otherwise it spends `ms` and yields `then`.
    Slow {
        clock: Arc<FixedClock>,
        ms: u64,
        then: Box<Scripted>,
    },
}

impl Scripted {
    pub fn slow(clock: &Arc<FixedClock>, ms: u64, then: Scripted) -> Self {
        Scripted::Slow {
            clock: Arc::clone(clock),
            ms,
            then: Box::new(then),
        }
    }

    fn resolve(self, request: &Request) -> std::result::Result<Response, TransportError> {
        match self {
            Scripted::Respond(response) => Ok(response),
            Scripted::Fail(err) => Err(err),
            Scripted::Custom(make) => make(),
            Scripted::Slow { clock, ms, then } => {
                if ms > request.deadline_ms {
                    clock.advance(request.deadline_ms);
                    return Err(TransportError {
                        kind: TransportFailure::Timeout,
                        detail: "request deadline reached".to_string(),
                    });
                }
                clock.advance(ms);
                then.resolve(request)
            }
        }
    }

    pub fn body(status: u16, body: Vec<u8>) -> Self {
        let length = body.len() as u64;
        Scripted::Respond(Response {
            status,
            location: None,
            content_length: Some(length),
            body: Box::new(ChunkReader::new(body, None)),
        })
    }

    pub fn body_without_length(status: u16, body: Vec<u8>) -> Self {
        Scripted::Respond(Response {
            status,
            location: None,
            content_length: None,
            body: Box::new(ChunkReader::new(body, None)),
        })
    }

    pub fn status(status: u16) -> Self {
        Self::body(status, Vec::new())
    }

    pub fn redirect(status: u16, location: &str) -> Self {
        Scripted::Respond(Response {
            status,
            location: Some(location.to_string()),
            content_length: Some(0),
            body: Box::new(ChunkReader::new(Vec::new(), None)),
        })
    }

    /// Declares `declared` bytes, delivers `prefix`, then fails with `kind`.
    pub fn broken(prefix: Vec<u8>, declared: u64, kind: io::ErrorKind) -> Self {
        Scripted::Respond(Response {
            status: 200,
            location: None,
            content_length: Some(declared),
            body: Box::new(ChunkReader::new(prefix, Some(kind))),
        })
    }

    /// Declares `declared` bytes but ends cleanly after `prefix`.
    pub fn short(prefix: Vec<u8>, declared: u64) -> Self {
        Scripted::Respond(Response {
            status: 200,
            location: None,
            content_length: Some(declared),
            body: Box::new(ChunkReader::new(prefix, None)),
        })
    }

    pub fn fail(kind: TransportFailure) -> Self {
        Scripted::Fail(TransportError {
            kind,
            detail: "scripted transport failure".to_string(),
        })
    }
}

#[derive(Default)]
pub struct ScriptedTransport {
    routes: Mutex<HashMap<String, VecDeque<Scripted>>>,
    log: Mutex<Vec<Request>>,
}

impl ScriptedTransport {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&self, url: &str, response: Scripted) -> &Self {
        self.routes
            .lock()
            .expect("routes lock")
            .entry(url.to_string())
            .or_default()
            .push_back(response);
        self
    }

    pub fn requests(&self) -> Vec<Request> {
        self.log.lock().expect("log lock").clone()
    }
}

impl Transport for ScriptedTransport {
    fn get(&self, request: &Request) -> std::result::Result<Response, TransportError> {
        self.log.lock().expect("log lock").push(request.clone());
        let next = self
            .routes
            .lock()
            .expect("routes lock")
            .get_mut(&request.url)
            .and_then(VecDeque::pop_front);
        match next {
            Some(scripted) => scripted.resolve(request),
            None => Err(TransportError {
                kind: TransportFailure::Other,
                detail: format!("no scripted response for {}", request.url),
            }),
        }
    }
}

// ----------------------------------------------------------------- stores

#[derive(Default)]
struct PartState {
    data: Vec<u8>,
    committed: Option<Vec<u8>>,
    discarded: bool,
    capacity: Option<usize>,
}

/// In-memory `.part` store with an optional capacity to model a full disk.
#[derive(Clone, Default)]
pub struct MemoryPartStore {
    state: Arc<Mutex<PartState>>,
}

impl MemoryPartStore {
    pub fn with_capacity(capacity: usize) -> Self {
        let store = Self::default();
        store.state.lock().expect("part lock").capacity = Some(capacity);
        store
    }

    pub fn committed(&self) -> Option<Vec<u8>> {
        self.state.lock().expect("part lock").committed.clone()
    }

    pub fn was_discarded(&self) -> bool {
        self.state.lock().expect("part lock").discarded
    }
}

struct PartWriter(Arc<Mutex<PartState>>);

impl Write for PartWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut state = self.0.lock().expect("part lock");
        if let Some(capacity) = state.capacity
            && state.data.len() + buf.len() > capacity
        {
            return Err(io::Error::new(io::ErrorKind::StorageFull, "disk full"));
        }
        state.data.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl PartStore for MemoryPartStore {
    fn begin_part(&mut self) -> io::Result<Box<dyn Write>> {
        let mut state = self.state.lock().expect("part lock");
        state.data.clear();
        state.discarded = false;
        Ok(Box::new(PartWriter(Arc::clone(&self.state))))
    }

    fn commit_part(&mut self) -> io::Result<()> {
        let mut state = self.state.lock().expect("part lock");
        let data = std::mem::take(&mut state.data);
        state.committed = Some(data);
        Ok(())
    }

    fn discard_part(&mut self) {
        let mut state = self.state.lock().expect("part lock");
        state.data.clear();
        state.discarded = true;
    }
}

#[derive(Default)]
struct SeenState {
    seen: Option<SeenCatalog>,
    fail_store: bool,
}

#[derive(Clone, Default)]
pub struct MemorySeenStore {
    state: Arc<Mutex<SeenState>>,
}

impl MemorySeenStore {
    pub fn with(seen: SeenCatalog) -> Self {
        let store = Self::default();
        store.state.lock().expect("seen lock").seen = Some(seen);
        store
    }

    pub fn fail_stores(&self) {
        self.state.lock().expect("seen lock").fail_store = true;
    }

    pub fn current(&self) -> Option<SeenCatalog> {
        self.state.lock().expect("seen lock").seen.clone()
    }
}

impl SeenStore for MemorySeenStore {
    fn load(&self) -> Result<Option<SeenCatalog>> {
        Ok(self.state.lock().expect("seen lock").seen.clone())
    }

    fn store(&mut self, seen: &SeenCatalog) -> Result<()> {
        let mut state = self.state.lock().expect("seen lock");
        if state.fail_store {
            return Err(UpdateError::new(ErrorCode::Io, "scripted store failure"));
        }
        state.seen = Some(seen.clone());
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fault {
    /// The store call fails and nothing is persisted.
    FailBefore,
    /// The new journal is persisted, then the call reports failure (a crash
    /// right after the flush looks like this to the caller).
    FailAfter,
}

#[derive(Default)]
struct FaultState {
    journal: Option<UpdateJournal>,
    faults: VecDeque<Option<Fault>>,
}

/// Journal store with scripted per-call faults; clones share state so a test
/// can "restart" by reopening it.
#[derive(Clone, Default)]
pub struct FaultyJournalStore {
    state: Arc<Mutex<FaultState>>,
}

impl FaultyJournalStore {
    /// One entry per upcoming `store` call; `None` means success.
    pub fn script(&self, faults: Vec<Option<Fault>>) {
        self.state.lock().expect("journal lock").faults = faults.into();
    }

    pub fn stored(&self) -> Option<UpdateJournal> {
        self.state.lock().expect("journal lock").journal.clone()
    }
}

impl JournalStore for FaultyJournalStore {
    fn load(&self) -> Result<Option<UpdateJournal>> {
        Ok(self.state.lock().expect("journal lock").journal.clone())
    }

    fn store(&mut self, journal: &UpdateJournal) -> Result<()> {
        let mut state = self.state.lock().expect("journal lock");
        match state.faults.pop_front().flatten() {
            Some(Fault::FailBefore) => Err(UpdateError::new(ErrorCode::Io, "scripted flush failure")),
            Some(Fault::FailAfter) => {
                state.journal = Some(journal.clone());
                Err(UpdateError::new(ErrorCode::Io, "scripted failure after flush"))
            }
            None => {
                state.journal = Some(journal.clone());
                Ok(())
            }
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct MemoryDisk {
    files: HashMap<String, Vec<u8>>,
}

impl MemoryDisk {
    pub fn set(&mut self, path: &str, bytes: &[u8]) {
        self.files.insert(path.to_string(), bytes.to_vec());
    }

    pub fn remove(&mut self, path: &str) {
        self.files.remove(path);
    }
}

impl DiskView for MemoryDisk {
    fn observe(&self, rel_path: &str) -> Result<Observed> {
        Ok(match self.files.get(rel_path) {
            None => Observed::Missing,
            Some(bytes) => Observed::File(FileIdentity::of(bytes)),
        })
    }
}

// -------------------------------------------------------------------- ZIP

fn put16(out: &mut Vec<u8>, value: u16) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn put32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = flate2::Crc::new();
    crc.update(data);
    crc.sum()
}

fn deflate(data: &[u8]) -> Vec<u8> {
    let mut encoder =
        flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(data).expect("deflate");
    encoder.finish().expect("deflate finish")
}

/// One ZIP entry with every header field adjustable, so tests can build both
/// well-formed archives and hostile ones.
#[derive(Debug, Clone)]
pub struct ZipEntry {
    pub name: Vec<u8>,
    pub data: Vec<u8>,
    pub method: u16,
    pub flags: u16,
    pub host: u8,
    pub attributes: u32,
    pub extra: Vec<u8>,
    pub declared_size: Option<u32>,
    pub crc: Option<u32>,
    pub local_name: Option<Vec<u8>>,
}

impl ZipEntry {
    pub fn file(name: &str, data: &[u8]) -> Self {
        Self {
            name: name.as_bytes().to_vec(),
            data: data.to_vec(),
            method: 8,
            flags: 0,
            host: 3,
            attributes: 0o100_644 << 16,
            extra: Vec::new(),
            declared_size: None,
            crc: None,
            local_name: None,
        }
    }

    pub fn dir(name: &str) -> Self {
        let mut entry = Self::file(name, &[]);
        entry.method = 0;
        entry.attributes = (0o040_755 << 16) | 0x10;
        entry
    }

    pub fn stored(mut self) -> Self {
        self.method = 0;
        self
    }

    pub fn mode(mut self, mode: u32) -> Self {
        self.attributes = (self.attributes & 0xffff) | (mode << 16);
        self
    }

    pub fn executable(self) -> Self {
        self.mode(0o100_755)
    }

    pub fn raw_name(mut self, name: &[u8]) -> Self {
        self.name = name.to_vec();
        self
    }

    pub fn is_dir(&self) -> bool {
        self.name.ends_with(b"/")
    }

    /// Size the archive will declare (and the catalog counts).
    pub fn declared(&self) -> u64 {
        u64::from(self.declared_size.unwrap_or(self.data.len() as u32))
    }
}

pub fn build_zip(entries: &[ZipEntry]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut central = Vec::new();
    for entry in entries {
        let payload = match entry.method {
            8 => deflate(&entry.data),
            _ => entry.data.clone(),
        };
        let crc = entry.crc.unwrap_or_else(|| crc32(&entry.data));
        let compressed = if entry.method == 0 {
            entry.declared_size.unwrap_or(payload.len() as u32)
        } else {
            payload.len() as u32
        };
        let size = entry.declared_size.unwrap_or(entry.data.len() as u32);
        let offset = out.len() as u32;
        let local_name = entry.local_name.as_ref().unwrap_or(&entry.name);

        put32(&mut out, 0x0403_4b50);
        put16(&mut out, 20);
        put16(&mut out, entry.flags);
        put16(&mut out, entry.method);
        put16(&mut out, 0);
        put16(&mut out, 33);
        put32(&mut out, crc);
        put32(&mut out, compressed);
        put32(&mut out, size);
        put16(&mut out, local_name.len() as u16);
        put16(&mut out, entry.extra.len() as u16);
        out.extend_from_slice(local_name);
        out.extend_from_slice(&entry.extra);
        out.extend_from_slice(&payload);

        put32(&mut central, 0x0201_4b50);
        put16(&mut central, (u16::from(entry.host) << 8) | 20);
        put16(&mut central, 20);
        put16(&mut central, entry.flags);
        put16(&mut central, entry.method);
        put16(&mut central, 0);
        put16(&mut central, 33);
        put32(&mut central, crc);
        put32(&mut central, compressed);
        put32(&mut central, size);
        put16(&mut central, entry.name.len() as u16);
        put16(&mut central, entry.extra.len() as u16);
        put16(&mut central, 0);
        put16(&mut central, 0);
        put16(&mut central, 0);
        put32(&mut central, entry.attributes);
        put32(&mut central, offset);
        central.extend_from_slice(&entry.name);
        central.extend_from_slice(&entry.extra);
    }
    let cd_offset = out.len() as u32;
    let cd_size = central.len() as u32;
    out.extend_from_slice(&central);
    put32(&mut out, 0x0605_4b50);
    put16(&mut out, 0);
    put16(&mut out, 0);
    put16(&mut out, entries.len() as u16);
    put16(&mut out, entries.len() as u16);
    put32(&mut out, cd_size);
    put32(&mut out, cd_offset);
    put16(&mut out, 0);
    out
}

// --------------------------------------------------------------- releases

pub const PUBLISHED_AT: &str = "2026-10-10T00:00:00Z";
pub const EXPIRES_AT: &str = "2027-01-01T00:00:00Z";

pub fn catalog_doc(version: &str, sequence: u64, target: Target, entry: TargetEntry) -> CatalogDocument {
    let mut targets = std::collections::BTreeMap::new();
    targets.insert(target.triple().to_string(), entry);
    CatalogDocument {
        schema: CATALOG_SCHEMA,
        channel: CHANNEL_STABLE.to_string(),
        repository: REPOSITORY.to_string(),
        version: version.to_string(),
        tag: format!("v{version}"),
        source_sha: TEST_SOURCE_SHA.to_string(),
        catalog_sequence: sequence,
        published_at: PUBLISHED_AT.to_string(),
        expires_at: EXPIRES_AT.to_string(),
        updater_protocol: UPDATER_PROTOCOL,
        min_updater_protocol: 1,
        state_compatibility: STATE_COMPATIBILITY_V1.to_string(),
        targets,
    }
}

/// Serialized catalog and its test-key signature file.
pub fn sign_catalog(doc: &CatalogDocument) -> (Vec<u8>, Vec<u8>) {
    let bytes = serde_json::to_vec(doc).expect("serialize catalog");
    let signature = sign(CATALOG_DOMAIN, &bytes);
    (bytes, signature)
}

pub fn macos_entry(version: &str, zip: &[u8], unpacked_size: u64) -> TargetEntry {
    TargetEntry {
        asset_name: Target::MacosArm64.asset_name().to_string(),
        archive_size: zip.len() as u64,
        archive_sha256: sha256_hex(zip),
        unpacked_size,
        layout_revision: Target::MacosArm64.layout_revision(),
        inventory_sha256: None,
        bundle_id: Some(MACOS_BUNDLE_ID.to_string()),
        team_id: Some(TEST_TEAM_ID.to_string()),
        bundle_version: Some(version.to_string()),
    }
}

pub struct WindowsRelease {
    pub zip: Vec<u8>,
    pub catalog: Vec<u8>,
    pub catalog_sig: Vec<u8>,
    pub inventory: Vec<u8>,
    pub inventory_sig: Vec<u8>,
    pub doc: CatalogDocument,
}

pub fn default_payload(version: &str) -> Vec<ZipEntry> {
    vec![
        ZipEntry::file("hane.exe", format!("hane.exe {version}").as_bytes()),
        ZipEntry::file(
            "hane-updater.exe",
            format!("hane-updater.exe {version}").as_bytes(),
        ),
        ZipEntry::dir("shell-integration/"),
        ZipEntry::dir(&format!("shell-integration/{version}/")),
        ZipEntry::file(
            &format!("shell-integration/{version}/hane_shell_extension.dll"),
            format!("dll {version}").as_bytes(),
        ),
        ZipEntry::file("README.md", b"readme"),
    ]
}

pub fn windows_release(version: &str, sequence: u64, payload: Vec<ZipEntry>) -> WindowsRelease {
    windows_release_with(version, sequence, payload, None)
}

/// Builds a signed Windows release. `inventory_files` lets a test make the
/// signed inventory disagree with the archive contents.
pub fn windows_release_with(
    version: &str,
    sequence: u64,
    payload: Vec<ZipEntry>,
    inventory_files: Option<Vec<(String, Vec<u8>)>>,
) -> WindowsRelease {
    let listed: Vec<(String, Vec<u8>)> = inventory_files.unwrap_or_else(|| {
        payload
            .iter()
            .filter(|e| !e.is_dir())
            .map(|e| (String::from_utf8_lossy(&e.name).into_owned(), e.data.clone()))
            .collect()
    });
    let inventory = Inventory {
        schema: INVENTORY_SCHEMA,
        version: version.to_string(),
        target: Target::WindowsX64.triple().to_string(),
        source_sha: TEST_SOURCE_SHA.to_string(),
        updater_protocol: UPDATER_PROTOCOL,
        layout_revision: Target::WindowsX64.layout_revision(),
        files: listed
            .iter()
            .map(|(path, data)| InventoryFile {
                path: path.clone(),
                size: data.len() as u64,
                sha256: sha256_hex(data),
            })
            .collect(),
    };
    let inventory_raw = serde_json::to_vec(&inventory).expect("serialize inventory");
    let inventory_sig = sign(INVENTORY_DOMAIN, &inventory_raw);

    let mut entries = payload;
    entries.push(ZipEntry::file(INVENTORY_FILE_NAME, &inventory_raw));
    entries.push(ZipEntry::file(INVENTORY_SIGNATURE_FILE_NAME, &inventory_sig));
    let unpacked: u64 = entries.iter().map(ZipEntry::declared).sum();
    let zip = build_zip(&entries);

    let entry = TargetEntry {
        asset_name: Target::WindowsX64.asset_name().to_string(),
        archive_size: zip.len() as u64,
        archive_sha256: sha256_hex(&zip),
        unpacked_size: unpacked,
        layout_revision: Target::WindowsX64.layout_revision(),
        inventory_sha256: Some(sha256_hex(&inventory_raw)),
        bundle_id: None,
        team_id: None,
        bundle_version: None,
    };
    let doc = catalog_doc(version, sequence, Target::WindowsX64, entry);
    let (catalog, catalog_sig) = sign_catalog(&doc);
    WindowsRelease {
        zip,
        catalog,
        catalog_sig,
        inventory: inventory_raw,
        inventory_sig,
        doc,
    }
}

/// GitHub `releases/latest` JSON for a tag with the given asset names.
pub fn release_json(tag: &str, assets: &[&str], draft: bool, prerelease: bool) -> Vec<u8> {
    let assets: Vec<serde_json::Value> = assets
        .iter()
        .map(|name| {
            serde_json::json!({
                "name": name,
                "size": 1234,
                "state": "uploaded",
                "browser_download_url":
                    crate::download::asset_url(tag, name),
            })
        })
        .collect();
    serde_json::to_vec(&serde_json::json!({
        "id": 42,
        "tag_name": tag,
        "draft": draft,
        "prerelease": prerelease,
        "assets": assets,
    }))
    .expect("serialize release")
}
