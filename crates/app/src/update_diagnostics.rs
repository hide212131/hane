//! Local diagnostics for application update failures (Issue #553).
//!
//! A failed update used to leave only a short code such as `io`. This module
//! keeps, on this machine only, a small bounded text of what failed: the
//! stage, the code, the internal detail and the OS error facts that were
//! available where the error was created. Nothing is sent anywhere.
//!
//! * Every value passes [`Redactor`] before it is stored, so the saved file and
//!   the text shown to the user follow the same rule: URL query/userinfo,
//!   credential-looking pairs, and user names inside paths are masked.
//! * The record count and total size are capped, and text is cut on character
//!   boundaries.
//! * Each record carries the id of the run (process instance) that wrote it,
//!   so a failure of the current run is never mixed up with records that were
//!   left by an earlier run.
//! * If the file cannot be written the records of this run stay in memory, and
//!   the report says that they were not saved. Updates are never reported as
//!   successful because of it, and nothing else is written for the log's sake.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

use hane_update::error::io_detail;
use hane_update::fsutil::durable_write;

pub(crate) const FILE_NAME: &str = "diagnostics.log";
pub(crate) const MAX_DETAIL_CHARS: usize = 400;
pub(crate) const MAX_RECORDS: usize = 20;
pub(crate) const MAX_FILE_BYTES: usize = 16 * 1024;

const REDACTED: &str = "<redacted>";
const USER_MASK: &str = "<user>";

/// Masks what must not be stored or shared. It never decides success or
/// failure of anything; it only rewrites text.
#[derive(Clone, Debug, Default)]
pub(crate) struct Redactor {
    home: Option<String>,
    user: Option<String>,
}

impl Redactor {
    pub(crate) fn new(home: Option<String>, user: Option<String>) -> Self {
        Self {
            home: home.filter(|home| home.len() > 1),
            user: user.filter(|user| user.chars().count() >= 3 && user != "root"),
        }
    }

    pub(crate) fn from_environment() -> Self {
        let var = |names: &[&str]| {
            names
                .iter()
                .find_map(|name| std::env::var(name).ok().filter(|value| !value.is_empty()))
        };
        Self::new(var(&["HOME", "USERPROFILE"]), var(&["USER", "USERNAME"]))
    }

    /// One line of at most `max_chars` characters (plus an ellipsis).
    pub(crate) fn redact(&self, text: &str, max_chars: usize) -> String {
        let mut out: Vec<String> = Vec::new();
        let mut mask_next = 0_u8;
        let tokens: Vec<&str> = text.split_whitespace().collect();
        for (index, token) in tokens.iter().copied().enumerate() {
            if mask_next > 0 {
                mask_next -= 1;
                out.push(REDACTED.to_owned());
                continue;
            }
            match token.to_ascii_lowercase().trim_end_matches(':') {
                "authorization" => mask_next = 2,
                "bearer" | "basic" => {
                    mask_next = 1;
                }
                _ => {}
            }
            let (text, mask_following) = self.redact_token(token);
            if mask_next == 0 {
                // `Proxy-Authorization: Bearer abc`: the scheme is its own
                // token, so the credential is the one after it.
                let scheme_next = tokens
                    .get(index + 1)
                    .is_some_and(|next| is_auth_scheme(next));
                if mask_following {
                    mask_next = if scheme_next { 2 } else { 1 };
                } else if !token.contains("://") && secret_pair_value_is_scheme(token) {
                    // `authorization:bearer abc`: the scheme sits inside the pair.
                    mask_next = 1;
                }
            }
            out.push(text);
        }
        truncate_chars(&out.join(" "), max_chars)
    }

    /// The masked token, and whether the value of a secret key continues in
    /// the next whitespace-separated token (`token: abc`).
    fn redact_token(&self, token: &str) -> (String, bool) {
        // A URL is handled as a whole: its query and userinfo are dropped.
        if !token.contains("://")
            && let Some(masked) = mask_secret_pair(token)
        {
            return masked;
        }
        let mut text = strip_url_secrets(token);
        if let Some(home) = &self.home {
            text = text.replace(home.as_str(), "~");
        }
        let mut text = mask_user_segments(&text);
        if let Some(user) = &self.user {
            text = text.replace(user.as_str(), USER_MASK);
        }
        (text, false)
    }
}

fn is_auth_scheme(word: &str) -> bool {
    const SCHEMES: [&str; 5] = ["bearer", "basic", "digest", "negotiate", "ntlm"];
    let lower = word.to_ascii_lowercase();
    SCHEMES.contains(&lower.as_str())
}

/// Whether the last secret pair of a token has only an auth scheme as its
/// value (`authorization:bearer`), so the credential is the next token.
fn secret_pair_value_is_scheme(token: &str) -> bool {
    let Some(segment) = token.rsplit([',', '&', ';']).next() else {
        return false;
    };
    let Some(at) = segment.find(['=', ':']) else {
        return false;
    };
    let Some((_, continues)) = mask_secret_segment(segment) else {
        return false;
    };
    let value = segment[at + 1..]
        .trim_matches(|c: char| "{[(\"'`}]) \t".contains(c));
    !continues && is_auth_scheme(value)
}

/// Masks every `key=value` / `key:value` / `"key":"value"` pair of a token
/// whose key looks secret. Pairs are split on `,` `&` `;`.
fn mask_secret_pair(token: &str) -> Option<(String, bool)> {
    let mut out = String::new();
    let mut changed = false;
    let mut mask_following = false;
    for segment in token.split_inclusive([',', '&', ';']) {
        let body = segment.trim_end_matches([',', '&', ';']);
        let delimiter = &segment[body.len()..];
        match mask_secret_segment(body) {
            Some((masked, continues)) => {
                changed = true;
                mask_following = continues && delimiter.is_empty();
                out.push_str(&masked);
            }
            None => out.push_str(body),
        }
        out.push_str(delimiter);
    }
    changed.then_some((out, mask_following))
}

fn mask_secret_segment(body: &str) -> Option<(String, bool)> {
    const WORDS: [&str; 10] = [
        "token",
        "secret",
        "password",
        "passwd",
        "apikey",
        "api_key",
        "api-key",
        "auth",
        "credential",
        "key",
    ];
    const WRAPPERS: [char; 8] = ['{', '[', '(', '"', '\'', '`', ' ', '\t'];
    let at = body.find(['=', ':'])?;
    let key_raw = &body[..at];
    let separator = &body[at..=at];
    let value = &body[at + 1..];
    let lower = key_raw.trim_matches(WRAPPERS).to_ascii_lowercase();
    let signature = lower.contains("signature") || lower == "sig";
    if !(signature || WORDS.iter().any(|word| lower.contains(word))) {
        return None;
    }
    let bare = value.trim_matches(|c: char| WRAPPERS.contains(&c) || "}])".contains(c));
    if bare.is_empty() {
        // `token:` / `"token":` — the value is the next token.
        return Some((body.to_owned(), true));
    }
    // Keep closing brackets so the surrounding structure stays readable.
    let closers: String = value
        .chars()
        .rev()
        .take_while(|c| "}])\"'`".contains(*c))
        .filter(|c| "}])".contains(*c))
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    // An opening quote without its closing one: the value has spaces.
    let unterminated = value
        .trim_start_matches(['{', '[', '('])
        .chars()
        .next()
        .filter(|c| matches!(c, '"' | '\''))
        .is_some_and(|quote| value.matches(quote).count() < 2);
    Some((
        format!("{key_raw}{separator}{REDACTED}{closers}"),
        unterminated,
    ))
}

/// Drops `user:password@` and everything after `?` or `#` of a URL.
fn strip_url_secrets(token: &str) -> String {
    let Some(sep) = token.find("://") else {
        return token.to_owned();
    };
    let (head, rest) = token.split_at(sep + 3);
    let cut = rest.find(['?', '#']);
    let (rest, had_query) = match cut {
        Some(index) => (&rest[..index], true),
        None => (rest, false),
    };
    let (authority, path) = rest.split_at(rest.find('/').unwrap_or(rest.len()));
    let host = authority.rsplit('@').next().unwrap_or(authority);
    let suffix = if had_query { "?<redacted>" } else { "" };
    format!("{head}{host}{path}{suffix}")
}

/// Replaces the segment after `/Users/`, `/home/` and the Windows forms.
fn mask_user_segments(text: &str) -> String {
    let mut out = text.to_owned();
    for marker in ["/Users/", "/home/", "\\Users\\", "\\users\\"] {
        let mut from = 0;
        while let Some(found) = out[from..].find(marker) {
            let start = from + found + marker.len();
            let end = out[start..]
                .find(['/', '\\'])
                .map_or(out.len(), |index| start + index);
            out.replace_range(start..end, USER_MASK);
            from = start + USER_MASK.len();
        }
    }
    out
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_owned();
    }
    let mut out: String = text.chars().take(max_chars).collect();
    out.push('…');
    out
}

/// A stable, non-reversible short id, so redacted paths still tell two
/// installs or state directories apart.
pub(crate) fn short_id(text: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in text.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")[..12].to_owned()
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

/// `YYYY-MM-DDTHH:MM:SSZ` for a Unix time.
pub(crate) fn iso_utc(secs: u64) -> String {
    let days = i64::try_from(secs / 86_400).unwrap_or(0);
    let rem = secs % 86_400;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

/// Keeps the newest records within the count and byte caps.
fn trim_records(lines: &mut Vec<String>) {
    while lines.len() > 1
        && (lines.len() > MAX_RECORDS
            || lines.iter().map(|line| line.len() + 1).sum::<usize>() > MAX_FILE_BYTES)
    {
        lines.remove(0);
    }
}

/// Who is writing: enough to tell several Hane processes and state
/// directories apart without the (redacted) paths.
pub(crate) struct Identity {
    /// Unique per service instance; marks which run wrote a record.
    pub run: String,
    pub pid: u32,
    pub exe: String,
    pub install: String,
    pub state: String,
    pub version: String,
}

#[derive(Default)]
struct Inner {
    /// Records of this run, kept even when they cannot be saved.
    session: Vec<String>,
    /// Records not yet written to the file.
    unsaved: Vec<String>,
    persist_error: Option<String>,
}

pub(crate) struct Diagnostics {
    path: PathBuf,
    identity: Identity,
    redactor: Redactor,
    /// Whether the file held records from an earlier run when this run began.
    /// It only makes the report reachable; it never describes a current failure.
    has_past: bool,
    inner: Mutex<Inner>,
    /// Serialises file writes without holding `inner` during file I/O.
    io: Mutex<()>,
}

impl Diagnostics {
    pub(crate) fn new(dir: &Path, identity: Identity, redactor: Redactor) -> Self {
        let mut diag = Self {
            path: dir.join(FILE_NAME),
            identity,
            redactor,
            has_past: false,
            inner: Mutex::new(Inner::default()),
            io: Mutex::new(()),
        };
        // Read once here (bounded by the file size cap), never per snapshot.
        diag.has_past = !diag.read_persisted().is_empty();
        diag
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Adds a record in memory only (cheap; safe under another lock). Call
    /// [`Self::flush`] afterwards, outside any lock, to save it.
    pub(crate) fn note(
        &self,
        stage: &str,
        code: &str,
        detail: &str,
        candidate_version: Option<&str>,
    ) {
        let line = format!(
            "{} run={} stage={} code={} version={} candidate={} detail={}",
            iso_utc(now_unix()),
            self.identity.run,
            self.redactor.redact(stage, 40),
            self.redactor.redact(code, 40),
            self.redactor.redact(&self.identity.version, 40),
            candidate_version.map_or_else(|| "-".to_owned(), |v| self.redactor.redact(v, 40)),
            self.redactor.redact(detail, MAX_DETAIL_CHARS),
        );
        let mut inner = self.lock();
        inner.session.push(line.clone());
        trim_records(&mut inner.session);
        inner.unsaved.push(line);
    }

    /// Writes the records added since the last flush. A failure is remembered
    /// for the report; it never affects the update itself.
    pub(crate) fn flush(&self) {
        let _io = self.io.lock().unwrap_or_else(PoisonError::into_inner);
        let batch = std::mem::take(&mut self.lock().unsaved);
        if batch.is_empty() {
            return;
        }
        let result = self.persist(batch.clone());
        let mut inner = self.lock();
        match result {
            Ok(()) => inner.persist_error = None,
            Err(error) => {
                // Put the batch back ahead of records added meanwhile, so a
                // later flush retries it in chronological order.
                let mut restored = batch;
                restored.append(&mut inner.unsaved);
                trim_records(&mut restored);
                inner.unsaved = restored;
                inner.persist_error =
                    Some(self.redactor.redact(&io_detail(&error), MAX_DETAIL_CHARS));
            }
        }
    }

    pub(crate) fn record(
        &self,
        stage: &str,
        code: &str,
        detail: &str,
        candidate_version: Option<&str>,
    ) {
        self.note(stage, code, detail, candidate_version);
        self.flush();
    }

    /// Whether there is anything to show: records of this run, or records an
    /// earlier run saved (listed separately in the report).
    pub(crate) fn has_records(&self) -> bool {
        self.has_past || !self.lock().session.is_empty()
    }

    fn persist(&self, batch: Vec<String>) -> io::Result<()> {
        let mut lines = self.read_persisted();
        lines.extend(batch);
        trim_records(&mut lines);
        if let Some(parent) = self.path.parent() {
            crate::app_update::create_private_dir(parent)?;
        }
        let mut text = lines.join("\n");
        text.push('\n');
        durable_write(&self.path, text.as_bytes())
    }

    /// The saved records. An absent, oversized or unreadable file counts as
    /// empty; it is replaced by the next successful save.
    fn read_persisted(&self) -> Vec<String> {
        match fs::metadata(&self.path) {
            Ok(meta) if meta.is_file() && meta.len() <= 2 * MAX_FILE_BYTES as u64 => {}
            _ => return Vec::new(),
        }
        fs::read(&self.path)
            .map(|bytes| {
                String::from_utf8_lossy(&bytes)
                    .lines()
                    .filter(|line| !line.trim().is_empty())
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The text the user can open or copy. Records of this run and records
    /// left by earlier runs are listed separately.
    pub(crate) fn report(&self) -> String {
        let (session, persist_error) = {
            let inner = self.lock();
            (inner.session.clone(), inner.persist_error.clone())
        };
        let past: Vec<String> = self
            .read_persisted()
            .into_iter()
            .filter(|line| line_run(line) != Some(self.identity.run.as_str()))
            .collect();
        let identity = &self.identity;
        let mut out = String::new();
        out.push_str("Hane 更新の診断情報\n");
        out.push_str(&format!("作成: {}\n", iso_utc(now_unix())));
        out.push_str(&format!("Hane: {}\n", identity.version));
        out.push_str(&format!("run: {} (PID {})\n", identity.run, identity.pid));
        out.push_str(&format!(
            "実行パス: {} [{}]\n",
            self.redactor.redact(&identity.exe, 300),
            short_id(&identity.exe)
        ));
        out.push_str(&format!("install: {}\n", identity.install));
        out.push_str(&format!("state: {}\n", identity.state));
        let saved_path = self.redactor.redact(&self.path.display().to_string(), 300);
        match (&persist_error, session.is_empty()) {
            (Some(reason), _) => out.push_str(&format!(
                "診断の保存: できませんでした ({reason})。下はこの起動中の記録のみで、再起動すると失われます。\n"
            )),
            (None, true) => out.push_str("診断の保存: この起動で記録された失敗はありません\n"),
            (None, false) => out.push_str(&format!("診断の保存: 保存済み ({saved_path})\n")),
        }
        out.push_str("\n## この起動の記録\n");
        if session.is_empty() {
            out.push_str("(なし)\n");
        }
        for line in &session {
            out.push_str(line);
            out.push('\n');
        }
        out.push_str("\n## 以前の起動の記録 (現在の失敗ではありません)\n");
        if past.is_empty() {
            out.push_str("(なし)\n");
        }
        for line in &past {
            out.push_str(line);
            out.push('\n');
        }
        out
    }
}

fn line_run(line: &str) -> Option<&str> {
    line.split_whitespace().nth(1)?.strip_prefix("run=")
}

#[cfg(test)]
mod tests {
    use super::*;
    use hane_update::testing::TempDir;

    fn identity(run: &str) -> Identity {
        Identity {
            run: run.to_owned(),
            pid: 4242,
            exe: "/Users/alice/Apps/Hane.app/Contents/MacOS/hane".to_owned(),
            install: "install-abc".to_owned(),
            state: "state-def".to_owned(),
            version: "0.26.0".to_owned(),
        }
    }

    fn redactor() -> Redactor {
        Redactor::new(Some("/Users/alice".to_owned()), Some("alice".to_owned()))
    }

    #[test]
    fn iso_utc_formats_known_instants() {
        assert_eq!(iso_utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(iso_utc(1_700_000_000), "2023-11-14T22:13:20Z");
    }

    #[test]
    fn urls_credentials_and_user_paths_are_masked() {
        let text = "GET https://user:pw@example.com/a/b?X-Amz-Signature=abc&t=1#frag failed \
                    token=sekret Authorization: Bearer abc.def \
                    /Users/alice/Library/x C:\\Users\\bob\\AppData";
        let out = redactor().redact(text, 1000);
        for leak in ["user:pw", "X-Amz", "abc&t", "sekret", "abc.def", "alice", "bob"] {
            assert!(!out.contains(leak), "{leak} leaked in {out}");
        }
        assert!(out.contains("https://example.com/a/b?<redacted>"), "{out}");
        assert!(out.contains("token=<redacted>"), "{out}");
        assert!(out.contains("~/Library/x"), "{out}");
    }

    #[test]
    fn auth_scheme_credentials_are_masked_after_header_keys() {
        let cases = [
            "Proxy-Authorization: Bearer abc",
            "authorization:bearer abc",
            "Proxy-Authorization: basic abc",
            "token: Bearer abc",
            "Authorization: Bearer abc",
        ];
        for input in cases {
            let out = redactor().redact(input, 1000);
            assert!(!out.contains("abc"), "{input} leaked in {out}");
        }
        let kept = redactor().redact("Proxy-Authorization: Bearer abc stage=check", 1000);
        assert!(kept.ends_with("stage=check"), "{kept}");
    }

    #[test]
    fn colon_and_json_style_secrets_are_masked() {
        let cases: [(&str, &str); 7] = [
            ("token:abc123", "token:<redacted>"),
            ("token: abc123 next", "token: <redacted> next"),
            ("\"token\":\"abc123\"", "\"token\":<redacted>"),
            ("\"token\": \"abc123\" ok", "\"token\": <redacted> ok"),
            ("{\"authToken\":\"abc123\"}", "{\"authToken\":<redacted>}"),
            (
                "{\"code\":\"E1\",\"password\":\"abc123\",\"stage\":\"check\"}",
                "{\"code\":\"E1\",\"password\":<redacted>,\"stage\":\"check\"}",
            ),
            ("a=1&token=abc123&b=2", "a=1&token=<redacted>&b=2"),
        ];
        for (input, expected) in cases {
            let out = redactor().redact(input, 1000);
            assert_eq!(out, expected, "{input}");
            assert!(!out.contains("abc123"), "leaked in {out}");
        }
        let plain = redactor().redact("stage=check code:E1 12:30:45", 1000);
        assert_eq!(plain, "stage=check code:E1 12:30:45");
    }

    #[test]
    fn long_japanese_text_is_cut_on_character_boundaries() {
        let text = "あ".repeat(1000);
        let out = redactor().redact(&text, MAX_DETAIL_CHARS);
        assert_eq!(out.chars().count(), MAX_DETAIL_CHARS + 1);
        assert!(out.ends_with('…'));
    }

    #[test]
    fn os_facts_are_kept_and_absent_numbers_are_not_invented() {
        let with_raw = io_detail(&io::Error::from_raw_os_error(2));
        assert!(with_raw.contains("os_error=2"), "{with_raw}");
        let without = io_detail(&io::Error::other("custom"));
        assert!(without.contains("os_error=none"), "{without}");
    }

    #[test]
    fn records_are_saved_bounded_and_separated_by_run() {
        let dir = TempDir::new("diag-bounded");
        let first = Diagnostics::new(dir.path(), identity("run-1"), redactor());
        for index in 0..(MAX_RECORDS * 3) {
            first.record("check", "io", &format!("failure {index} {}", "あ".repeat(380)), None);
        }
        let saved = fs::read_to_string(dir.path().join(FILE_NAME)).unwrap();
        assert!(saved.len() <= MAX_FILE_BYTES, "{}", saved.len());
        assert!(saved.lines().count() <= MAX_RECORDS);
        assert!(saved.contains("failure 59"));
        assert!(!saved.contains("failure 0 "));

        // A later run sees the earlier records only as past records.
        let second = Diagnostics::new(dir.path(), identity("run-2"), redactor());
        second.record("download", "hash_mismatch", "mismatch", Some("0.27.0"));
        let report = second.report();
        let (current, past) = report.split_once("## 以前の起動の記録").unwrap();
        assert!(current.contains("run=run-2") && !current.contains("run=run-1"));
        assert!(past.contains("run=run-1") && !past.contains("run=run-2"));
        assert!(!report.contains("alice"));
        assert!(report.contains("[") && report.contains("install-abc"));
    }

    #[test]
    fn a_unwritable_log_keeps_this_runs_records_and_says_so() {
        let dir = TempDir::new("diag-unwritable");
        // A directory where the file belongs makes only the log unwritable.
        fs::create_dir_all(dir.path().join(FILE_NAME)).unwrap();
        let diag = Diagnostics::new(dir.path(), identity("run-1"), redactor());
        diag.record("publish", "io", "rename failed", None);
        assert!(diag.has_records());
        let report = diag.report();
        assert!(report.contains("保存: できませんでした"), "{report}");
        assert!(report.contains("rename failed"));
    }

    #[test]
    fn a_failed_batch_is_retried_in_order_and_clears_the_error_on_success() {
        let dir = TempDir::new("diag-retry");
        fs::create_dir_all(dir.path().join(FILE_NAME)).unwrap();
        let diag = Diagnostics::new(dir.path(), identity("run-1"), redactor());
        diag.record("check", "io", "first failure", None);
        assert!(diag.report().contains("保存: できませんでした"));
        diag.record("check", "io", "second failure", None);
        assert!(diag.report().contains("保存: できませんでした"));

        fs::remove_dir_all(dir.path().join(FILE_NAME)).unwrap();
        diag.record("check", "io", "third failure", None);
        assert!(!diag.report().contains("保存: できませんでした"));

        let restarted = Diagnostics::new(dir.path(), identity("run-2"), redactor());
        let saved = fs::read_to_string(dir.path().join(FILE_NAME)).unwrap();
        let first = saved.find("first failure").expect("first saved");
        let second = saved.find("second failure").expect("second saved");
        let third = saved.find("third failure").expect("third saved");
        assert!(first < second && second < third, "{saved}");
        let report = restarted.report();
        assert!(report.contains("first failure") && report.contains("third failure"));
    }
}
