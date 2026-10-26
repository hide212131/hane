//! The state root of an update and the read-only check of the new version
//! (Issue #510, U4).
//!
//! The application that starts an update knows the real state root (the
//! default directory or the `HANE_STATE_DIR` override, whichever
//! `FileStateStore` resolved). The limited-health child starts with an empty
//! environment, so the root cannot travel as a variable; it would silently
//! fall back to another directory. Instead the parent publishes a *binding* in
//! the owner-only transaction directory before the helper exists
//! ([`publish_binding`]); the helper reads it ([`read_binding`]) and places the
//! root into the one-time health capability of the child, which is the only
//! way into health mode. No flag, environment variable or path argument names a
//! root, and a root is accepted only when it is absolute, normalized, a plain
//! directory owned by the current principal and closed to other writers
//! ([`validate_root`]).
//!
//! [`StateCheck::verify`] is what the child runs before it reports `Healthy`.
//! It only reads: the root must still be the bound one, the settings store
//! must open, and the transaction's `restart-ticket.json` must match this
//! transaction, installation and root and stay within its schema and limits.
//! A missing, damaged or foreign ticket is a failure; nothing here creates or
//! repairs one.

use std::fs;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{ErrorCode, Result, UpdateError};
use crate::fsutil::{durable_write, read_limited, rename_new_durable, sync_renamed_file};
use crate::identity::{InstallId, TransactionId};
use crate::journal::{FileJournalStore, JournalStore, Phase};
use crate::platform::acl;
use crate::platform::lease::Coordination;
use crate::util::parse_strict;

pub const BINDING_FILE: &str = "state-root.binding";
pub const TICKET_FILE: &str = "restart-ticket.json";
/// The ticket after the new version took it. Its existence means the ticket was
/// already processed, so a second start never restores the same files again.
pub const CLAIMED_FILE: &str = "restart-ticket.claimed";
pub const BINDING_SCHEMA: u32 = 1;
pub const TICKET_SCHEMA: u32 = 1;

/// Longest state root accepted anywhere on the way to the child.
pub const MAX_ROOT_BYTES: usize = 1024;
const BINDING_LIMIT: u64 = 4096;
const TICKET_LIMIT: u64 = 64 * 1024;
const SETTINGS_FILE: &str = "settings.conf";
const SETTINGS_LIMIT: u64 = 1024 * 1024;
pub const MAX_TICKET_FILES: usize = 64;
pub const MAX_TICKET_PATH_BYTES: usize = 2048;
pub const MAX_POSITION: u32 = 100_000_000;

fn mismatch(detail: impl Into<String>) -> UpdateError {
    UpdateError::new(ErrorCode::IdentityMismatch, detail)
}

/// Accepts a state root only when it is absolute, normalized (no `.`/`..`, no
/// doubled or trailing separators) and a real directory that belongs to the
/// current principal and is not writable by anyone else.
pub fn validate_root(path: &Path) -> Result<PathBuf> {
    let text = path
        .to_str()
        .ok_or_else(|| mismatch("the state root is not valid text"))?;
    if text.is_empty() || text.len() > MAX_ROOT_BYTES || text.chars().any(char::is_control) {
        return Err(mismatch("the state root is empty, too long or has control characters"));
    }
    if !path.is_absolute() {
        return Err(mismatch("the state root is not an absolute path"));
    }
    if path
        .components()
        .any(|part| matches!(part, Component::CurDir | Component::ParentDir))
    {
        return Err(mismatch("the state root is not normalized"));
    }
    let normalized: PathBuf = path.components().collect();
    if normalized.as_os_str() != path.as_os_str() {
        return Err(mismatch("the state root is not normalized"));
    }
    acl::verify(path, true)?;
    Ok(path.to_path_buf())
}

fn read_private(path: &Path, limit: u64, what: &str) -> Result<Vec<u8>> {
    acl::verify(path, false)?;
    read_limited(path, limit)?
        .ok_or_else(|| UpdateError::new(ErrorCode::NotFound, format!("{what} is missing")))
}

// --------------------------------------------------------------- binding

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Binding {
    schema: u32,
    install_id: String,
    transaction_id: String,
    state_root: String,
}

fn binding_path(coordination: &Coordination, transaction: &TransactionId) -> PathBuf {
    coordination.transaction_dir(transaction).join(BINDING_FILE)
}

/// Parent side: binds `root` to this installation and transaction in the
/// owner-only transaction directory. The root is validated first; a root that
/// cannot be validated is never published.
pub fn publish_binding(
    coordination: &Coordination,
    transaction: &TransactionId,
    root: &Path,
) -> Result<()> {
    let root = validate_root(root)?;
    let state_root = root
        .to_str()
        .ok_or_else(|| mismatch("the state root is not valid text"))?
        .to_owned();
    let binding = Binding {
        schema: BINDING_SCHEMA,
        install_id: coordination.install_id().as_str().to_owned(),
        transaction_id: transaction.as_str().to_owned(),
        state_root,
    };
    let bytes = serde_json::to_vec(&binding)
        .map_err(|err| UpdateError::new(ErrorCode::Io, err.to_string()))?;
    discard_binding(coordination, transaction);
    let path = binding_path(coordination, transaction);
    durable_write(&path, &bytes)?;
    if let Err(err) = acl::secure_new(&path, false, 0o600).and_then(|()| acl::verify(&path, false)) {
        let _ = fs::remove_file(&path);
        return Err(err);
    }
    Ok(())
}

/// Removes a binding that is no longer wanted.
pub fn discard_binding(coordination: &Coordination, transaction: &TransactionId) {
    let _ = fs::remove_file(binding_path(coordination, transaction));
}

/// Helper side: reads the published root without changing anything. An absent,
/// foreign, damaged or no longer valid binding is an error.
pub fn read_binding(coordination: &Coordination, transaction: &TransactionId) -> Result<PathBuf> {
    let bytes = read_private(
        &binding_path(coordination, transaction),
        BINDING_LIMIT,
        "the state root binding",
    )?;
    let binding: Binding = parse_strict(&bytes)?;
    if binding.schema != BINDING_SCHEMA {
        return Err(UpdateError::new(
            ErrorCode::SchemaUnsupported,
            "unsupported state root binding",
        ));
    }
    if binding.install_id != coordination.install_id().as_str()
        || binding.transaction_id != transaction.as_str()
    {
        return Err(mismatch("the state root binding is for another installation or transaction"));
    }
    validate_root(Path::new(&binding.state_root))
}

// ----------------------------------------------------------- environment

pub const ENVIRONMENT_FILE: &str = "child-env.json";
const ENVIRONMENT_LIMIT: u64 = 16 * 1024;
const MAX_ENV_VALUE_BYTES: usize = 1024;
const MAX_PATH_ENV_VALUE_BYTES: usize = 8 * 1024;

/// The only variables that may reach the limited child. They are what a normal
/// Hane start needs to find the same user profile (settings, AI configuration,
/// the named pipe derived from `LOCALAPPDATA`) and the Windows execution
/// environment the AI runtime passes to its commands. Loader variables
/// (`LD_PRELOAD`, `DYLD_*`, ...) are deliberately absent.
pub const CHILD_ENV_ALLOWLIST: &[&str] = &[
    "HOME",
    "USER",
    "LOGNAME",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "TMPDIR",
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
    "XDG_STATE_HOME",
    "XDG_CACHE_HOME",
    "HANE_STATE_DIR",
    "SystemRoot",
    "LOCALAPPDATA",
    "APPDATA",
    "USERPROFILE",
    "USERNAME",
    "USERDOMAIN",
    "HOMEDRIVE",
    "HOMEPATH",
    "TEMP",
    "TMP",
    "XDG_RUNTIME_DIR",
    "__CF_USER_TEXT_ENCODING",
    // Command execution environment the normal AI runtime allows, so the AI
    // tools of the first editor after an update can resolve and run commands.
    "PATH",
    "PATHEXT",
    "COMSPEC",
    "WINDIR",
    "PROCESSOR_ARCHITECTURE",
    // The editor discovery of the UI reads these to find a system-wide VS Code
    // that is on neither PATH nor App Paths.
    "ProgramFiles",
    "ProgramFiles(x86)",
    // Connectivity and certificate settings the AI runtime passes to its own
    // child, so the limited child reaches the same network as a normal start.
    "ALL_PROXY",
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "NO_PROXY",
    "CURL_CA_BUNDLE",
    "REQUESTS_CA_BUNDLE",
    "SSL_CERT_DIR",
    "SSL_CERT_FILE",
];

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ChildEnvironment {
    schema: u32,
    install_id: String,
    transaction_id: String,
    vars: std::collections::BTreeMap<String, String>,
    /// Variable name -> opaque, transaction-bound reference of the temporary OS
    /// secret holding the proxy `user:password` that was removed from that
    /// variable's URL. The reference is not a secret; the userinfo itself is
    /// never written to this file.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    secret_refs: std::collections::BTreeMap<String, String>,
}

/// The most temporary secrets one transaction may hold. It bounds the references
/// a terminal cleanup has to sweep.
const MAX_SECRET_REFS: usize = 8;

/// Proxy variables whose value is a URL that may carry `user:password@`.
const PROXY_URL_VARS: &[&str] = &["ALL_PROXY", "HTTP_PROXY", "HTTPS_PROXY"];

fn is_proxy_url_var(name: &str) -> bool {
    PROXY_URL_VARS
        .iter()
        .any(|proxy| name.eq_ignore_ascii_case(proxy))
}

/// Temporary OS secret store for the proxy userinfo of one restart
/// transaction (Keychain on macOS, Credential Manager on Windows). Entries are
/// addressed by an opaque reference derived from the transaction id and are
/// consumed exactly once. Implementations must never write a secret anywhere
/// but the OS store and must not put it into an error message.
pub trait TransientSecretStore {
    fn put(&self, reference: &str, secret: &str) -> Result<()>;
    /// Returns the secret and removes it (one-time claim). `Ok(None)` when no
    /// such entry exists.
    fn take(&self, reference: &str) -> Result<Option<String>>;
    /// Removing an absent entry is not an error.
    fn delete(&self, reference: &str) -> Result<()>;
}

/// The store used when no OS secret backend is supplied. Every `put` fails, so
/// a proxy with userinfo aborts the handoff instead of being written to a file
/// or restored without authentication.
pub struct UnavailableSecretStore;

fn secret_store_unavailable() -> UpdateError {
    UpdateError::new(
        ErrorCode::Io,
        "the proxy credential for the restart cannot be handed over safely",
    )
}

impl TransientSecretStore for UnavailableSecretStore {
    fn put(&self, _: &str, _: &str) -> Result<()> {
        Err(secret_store_unavailable())
    }

    fn take(&self, _: &str) -> Result<Option<String>> {
        Err(secret_store_unavailable())
    }

    fn delete(&self, _: &str) -> Result<()> {
        Ok(())
    }
}

/// The opaque reference of the `index`-th secret of `transaction`.
fn secret_ref(transaction: &TransactionId, index: usize) -> String {
    format!("{}.{index}", transaction.as_str())
}

fn secret_ref_ok(transaction: &TransactionId, reference: &str) -> bool {
    reference
        .strip_prefix(transaction.as_str())
        .and_then(|rest| rest.strip_prefix('.'))
        .is_some_and(|index| {
            !index.is_empty() && index.len() <= 4 && index.chars().all(|c| c.is_ascii_digit())
        })
}

/// Splits a proxy value into the value without userinfo and the userinfo. A
/// value with `@` only after the authority (path, query) is refused rather
/// than stored.
fn split_userinfo(value: &str) -> Result<(String, Option<String>)> {
    let (scheme, rest) = match value.find("://") {
        Some(index) => value.split_at(index + 3),
        None => ("", value),
    };
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, tail) = rest.split_at(end);
    match authority.rfind('@') {
        Some(at) => Ok((
            format!("{scheme}{}{tail}", &authority[at + 1..]),
            Some(authority[..at].to_owned()),
        )),
        None if tail.contains('@') => {
            Err(mismatch("a proxy value has credentials outside its authority"))
        }
        None => Ok((value.to_owned(), None)),
    }
}

fn join_userinfo(value: &str, userinfo: &str) -> String {
    match value.find("://") {
        Some(index) => {
            let (scheme, rest) = value.split_at(index + 3);
            format!("{scheme}{userinfo}@{rest}")
        }
        None => format!("{userinfo}@{value}"),
    }
}

fn environment_path(coordination: &Coordination, transaction: &TransactionId) -> PathBuf {
    coordination.transaction_dir(transaction).join(ENVIRONMENT_FILE)
}

/// Whether `name` is on the allowlist, ignoring ASCII case.
pub fn env_name_allowed(name: &str) -> bool {
    CHILD_ENV_ALLOWLIST
        .iter()
        .any(|allowed| name.eq_ignore_ascii_case(allowed))
}

/// The value limit for `name`. A Windows `PATH` routinely exceeds 1 KiB, so it
/// alone gets a larger limit; the serialized record stays under
/// `ENVIRONMENT_LIMIT` either way.
fn env_value_limit(name: &str) -> usize {
    if name.eq_ignore_ascii_case("PATH") {
        MAX_PATH_ENV_VALUE_BYTES
    } else {
        MAX_ENV_VALUE_BYTES
    }
}

fn env_value_ok(name: &str, value: &str) -> bool {
    !value.is_empty()
        && value.len() <= env_value_limit(name)
        && !value.chars().any(char::is_control)
}

/// Parent side: records the allowlisted variables of the real parent in the
/// owner-only transaction directory. Anything absent, not text, empty, too long
/// or with control characters is simply left out. Names are matched against the
/// allowlist ASCII case-insensitively, like the normal AI runtime does, and the
/// original spelling of the name is kept.
///
/// A proxy variable with `user:password@` never has its userinfo written: this
/// form fails with an error instead (the handoff is aborted and the running
/// version stays). Use [`publish_environment_with_secrets`] to carry the
/// userinfo through a temporary OS secret.
pub fn publish_environment(
    coordination: &Coordination,
    transaction: &TransactionId,
    ambient: impl IntoIterator<Item = (std::ffi::OsString, std::ffi::OsString)>,
) -> Result<()> {
    publish_environment_with_secrets(coordination, transaction, ambient, &UnavailableSecretStore)
}

/// Like [`publish_environment`], but the userinfo of a proxy URL is moved into
/// `secrets` under a reference bound to `transaction`; only that non-secret
/// reference is recorded. When anything fails, every secret stored so far is
/// deleted again and nothing is left behind.
pub fn publish_environment_with_secrets(
    coordination: &Coordination,
    transaction: &TransactionId,
    ambient: impl IntoIterator<Item = (std::ffi::OsString, std::ffi::OsString)>,
    secrets: &dyn TransientSecretStore,
) -> Result<()> {
    let mut vars = std::collections::BTreeMap::new();
    let mut secret_refs = std::collections::BTreeMap::new();
    let mut stored: Vec<String> = Vec::new();
    let result = collect_environment(
        transaction,
        ambient,
        secrets,
        &mut vars,
        &mut secret_refs,
        &mut stored,
    )
    .and_then(|()| write_environment(coordination, transaction, vars, secret_refs));
    if result.is_err() {
        for reference in &stored {
            let _ = secrets.delete(reference);
        }
    }
    result
}

fn collect_environment(
    transaction: &TransactionId,
    ambient: impl IntoIterator<Item = (std::ffi::OsString, std::ffi::OsString)>,
    secrets: &dyn TransientSecretStore,
    vars: &mut std::collections::BTreeMap<String, String>,
    secret_refs: &mut std::collections::BTreeMap<String, String>,
    stored: &mut Vec<String>,
) -> Result<()> {
    for (name, value) in ambient {
        let Ok(name) = name.into_string() else {
            continue;
        };
        if !env_name_allowed(&name) {
            continue;
        }
        let Ok(value) = value.into_string() else {
            continue;
        };
        let (value, userinfo) = if is_proxy_url_var(&name) {
            split_userinfo(&value)?
        } else {
            (value, None)
        };
        if !env_value_ok(&name, &value) {
            continue;
        }
        if let Some(userinfo) = userinfo.filter(|userinfo| !userinfo.is_empty()) {
            if stored.len() >= MAX_SECRET_REFS {
                return Err(UpdateError::new(
                    ErrorCode::LimitExceeded,
                    "too many proxy credentials for the restart",
                ));
            }
            let reference = secret_ref(transaction, stored.len());
            secrets.put(&reference, &userinfo)?;
            stored.push(reference.clone());
            secret_refs.insert(name.clone(), reference);
        }
        vars.insert(name, value);
    }
    Ok(())
}

fn write_environment(
    coordination: &Coordination,
    transaction: &TransactionId,
    vars: std::collections::BTreeMap<String, String>,
    secret_refs: std::collections::BTreeMap<String, String>,
) -> Result<()> {
    let document = ChildEnvironment {
        schema: BINDING_SCHEMA,
        install_id: coordination.install_id().as_str().to_owned(),
        transaction_id: transaction.as_str().to_owned(),
        vars,
        secret_refs,
    };
    let bytes = serde_json::to_vec(&document)
        .map_err(|err| UpdateError::new(ErrorCode::Io, err.to_string()))?;
    discard_environment(coordination, transaction);
    // The child re-reads this record with `ENVIRONMENT_LIMIT`; refuse before
    // writing anything a reader would reject.
    if bytes.len() as u64 > ENVIRONMENT_LIMIT {
        return Err(UpdateError::new(
            ErrorCode::LimitExceeded,
            "the child environment is too large",
        ));
    }
    let path = environment_path(coordination, transaction);
    durable_write(&path, &bytes)?;
    if let Err(err) = acl::secure_new(&path, false, 0o600).and_then(|()| acl::verify(&path, false)) {
        let _ = fs::remove_file(&path);
        return Err(err);
    }
    Ok(())
}

/// Removes a recorded environment that is no longer wanted.
pub fn discard_environment(coordination: &Coordination, transaction: &TransactionId) {
    let _ = fs::remove_file(environment_path(coordination, transaction));
}

/// Helper side: the recorded variables, re-checked against the allowlist and
/// the limits. A missing record means no variables; a damaged, foreign or
/// non-allowlisted one is an error.
///
/// A record that references a temporary proxy secret is refused here: without
/// the secret store the child must not start with an unauthenticated proxy. Use
/// [`read_environment_with_secrets`] for it.
pub fn read_environment(
    coordination: &Coordination,
    transaction: &TransactionId,
) -> Result<Vec<(String, String)>> {
    let (vars, secret_refs) = load_environment(coordination, transaction)?;
    if secret_refs.is_empty() {
        Ok(vars)
    } else {
        Err(secret_store_unavailable())
    }
}

/// Like [`read_environment`], but restores the proxy userinfo from `secrets`.
/// Every referenced secret is consumed exactly once, also when one of them is
/// missing; a missing or unreadable secret is an error, never an
/// unauthenticated proxy.
pub fn read_environment_with_secrets(
    coordination: &Coordination,
    transaction: &TransactionId,
    secrets: &dyn TransientSecretStore,
) -> Result<Vec<(String, String)>> {
    let (mut vars, secret_refs) = load_environment(coordination, transaction)?;
    let mut failed = false;
    for (name, reference) in &secret_refs {
        match secrets.take(reference) {
            Ok(Some(userinfo)) => {
                if let Some((_, value)) = vars.iter_mut().find(|(key, _)| key == name) {
                    *value = join_userinfo(value, &userinfo);
                }
            }
            Ok(None) | Err(_) => failed = true,
        }
    }
    if failed {
        Err(secret_store_unavailable())
    } else {
        Ok(vars)
    }
}

/// Deletes every temporary secret of `transaction` and then the record itself.
/// Used for every terminal path of a transaction: success, abort, rollback and
/// recovery.
///
/// The references are the ones the record names plus the bounded, deterministic
/// set a publish can have used, so a secret whose record was never written (a
/// failed publish) is found too. When any delete fails the record is kept and
/// an error is returned: the secret stays tracked and the caller must not drop
/// the transaction directory, so a later cleanup can retry. Deleting an absent
/// entry is not an error, which makes the call idempotent.
pub fn release_environment_secrets(
    coordination: &Coordination,
    transaction: &TransactionId,
    secrets: &dyn TransientSecretStore,
) -> Result<()> {
    let mut references: std::collections::BTreeSet<String> = (0..MAX_SECRET_REFS)
        .map(|index| secret_ref(transaction, index))
        .collect();
    let path = environment_path(coordination, transaction);
    if let Ok(Some(bytes)) = read_limited(&path, ENVIRONMENT_LIMIT)
        && let Ok(document) = parse_strict::<ChildEnvironment>(&bytes)
    {
        references.extend(
            document
                .secret_refs
                .into_values()
                .filter(|reference| secret_ref_ok(transaction, reference)),
        );
    }
    // Every delete is attempted even after a failure.
    let mut failed = false;
    for reference in &references {
        if secrets.delete(reference).is_err() {
            failed = true;
        }
    }
    if failed {
        return Err(UpdateError::new(
            ErrorCode::Io,
            "the temporary proxy credential could not be removed yet",
        ));
    }
    discard_environment(coordination, transaction);
    Ok(())
}

/// [`release_environment_secrets`] against the OS store. `true` means no secret
/// of the transaction is left, so the transaction directory may be removed;
/// `false` keeps it for the next cleanup.
pub fn release_transaction_secrets(coordination: &Coordination, transaction: &TransactionId) -> bool {
    release_environment_secrets(coordination, transaction, &crate::secret_store::OsSecretStore::proxy())
        .is_ok()
}

#[allow(clippy::type_complexity)]
fn load_environment(
    coordination: &Coordination,
    transaction: &TransactionId,
) -> Result<(
    Vec<(String, String)>,
    std::collections::BTreeMap<String, String>,
)> {
    let path = environment_path(coordination, transaction);
    match fs::symlink_metadata(&path) {
        Ok(_) => {}
        // Only a genuinely absent record means "no variables"; any other
        // metadata failure (I/O, ACL) must not be mistaken for an empty one.
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Ok((Vec::new(), std::collections::BTreeMap::new()));
        }
        Err(err) => return Err(err.into()),
    }
    let bytes = read_private(&path, ENVIRONMENT_LIMIT, "the child environment")?;
    let document: ChildEnvironment = parse_strict(&bytes)?;
    if document.schema != BINDING_SCHEMA {
        return Err(UpdateError::new(
            ErrorCode::SchemaUnsupported,
            "unsupported child environment",
        ));
    }
    if document.install_id != coordination.install_id().as_str()
        || document.transaction_id != transaction.as_str()
    {
        return Err(mismatch("the child environment is for another installation or transaction"));
    }
    let mut vars = Vec::new();
    for (name, value) in document.vars {
        if !env_name_allowed(&name) || !env_value_ok(&name, &value) {
            return Err(mismatch("the child environment names a variable that is not allowed"));
        }
        // A stored proxy value never carries credentials; a hand-written one
        // that does is refused.
        if is_proxy_url_var(&name) && !matches!(split_userinfo(&value), Ok((_, None))) {
            return Err(mismatch("the child environment holds proxy credentials"));
        }
        vars.push((name, value));
    }
    for (name, reference) in &document.secret_refs {
        let known = vars.iter().any(|(key, _)| key == name);
        if !known || !is_proxy_url_var(name) || !secret_ref_ok(transaction, reference) {
            return Err(mismatch("the child environment has an invalid secret reference"));
        }
    }
    Ok((vars, document.secret_refs))
}

// ---------------------------------------------------------------- ticket

/// One saved file of the restart ticket. Only a path and bounded positions:
/// never document text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TicketFile {
    pub path: String,
    pub scroll_line: u32,
    pub caret_offset: u32,
}

/// The Windows Explorer integration that was enabled before the update: the
/// owner and package identity read by the trusted inspection, and which of the
/// file and folder verbs were ON. At least one verb is enabled; a user without
/// the integration has no entry at all.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExplorerTicket {
    pub owner: String,
    pub package_name: String,
    pub publisher: String,
    pub file_enabled: bool,
    pub folder_enabled: bool,
}

const MAX_EXPLORER_FIELD_BYTES: usize = 256;

impl ExplorerTicket {
    fn validate(&self) -> Result<()> {
        let field_ok = |text: &str| {
            !text.is_empty()
                && text.len() <= MAX_EXPLORER_FIELD_BYTES
                && !text.chars().any(char::is_control)
        };
        if !field_ok(&self.owner) || !field_ok(&self.package_name) || !field_ok(&self.publisher) {
            return Err(UpdateError::new(
                ErrorCode::MalformedMetadata,
                "the restart ticket has an invalid Explorer registration".to_owned(),
            ));
        }
        if !self.file_enabled && !self.folder_enabled {
            return Err(UpdateError::new(
                ErrorCode::MalformedMetadata,
                "the restart ticket records an Explorer registration with nothing enabled".to_owned(),
            ));
        }
        Ok(())
    }
}

/// `restart-ticket.json` in the transaction directory. It carries no document
/// text, secret, command or environment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RestartTicket {
    pub schema: u32,
    pub install_id: String,
    pub transaction_id: String,
    pub state_root: String,
    pub work_folder: Option<String>,
    pub files: Vec<TicketFile>,
    /// Index into `files` of the file that was active.
    pub active: Option<u32>,
    /// The Explorer integration to carry to the new package version. `None`
    /// when the user had it off or never registered it.
    #[serde(default)]
    pub explorer: Option<ExplorerTicket>,
}

/// Whether `text` is acceptable as a path in a ticket: absolute, bounded, free
/// of control characters and of `.`/`..` components.
pub fn ticket_path_ok(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= MAX_TICKET_PATH_BYTES
        && !text.chars().any(char::is_control)
        && Path::new(text).is_absolute()
        && !Path::new(text)
            .components()
            .any(|part| matches!(part, Component::CurDir | Component::ParentDir))
}

impl RestartTicket {
    fn validate(&self, install: &InstallId, transaction: &TransactionId, root: &Path) -> Result<()> {
        if self.schema != TICKET_SCHEMA {
            return Err(UpdateError::new(
                ErrorCode::SchemaUnsupported,
                "unsupported restart ticket schema",
            ));
        }
        if self.install_id != install.as_str() || self.transaction_id != transaction.as_str() {
            return Err(mismatch("the restart ticket is for another installation or transaction"));
        }
        if Some(self.state_root.as_str()) != root.to_str() {
            return Err(mismatch("the restart ticket names another state root"));
        }
        let malformed = |detail: &str| UpdateError::new(ErrorCode::MalformedMetadata, detail.to_owned());
        if self.files.len() > MAX_TICKET_FILES {
            return Err(UpdateError::new(
                ErrorCode::LimitExceeded,
                "the restart ticket lists too many files",
            ));
        }
        if let Some(folder) = &self.work_folder
            && !ticket_path_ok(folder)
        {
            return Err(malformed("the restart ticket has an invalid work folder"));
        }
        for file in &self.files {
            if !ticket_path_ok(&file.path)
                || file.scroll_line > MAX_POSITION
                || file.caret_offset > MAX_POSITION
            {
                return Err(malformed("the restart ticket has an invalid file entry"));
            }
        }
        if let Some(active) = self.active
            && !usize::try_from(active).is_ok_and(|index| index < self.files.len())
        {
            return Err(malformed("the restart ticket's active file is not listed"));
        }
        if let Some(explorer) = &self.explorer {
            explorer.validate()?;
        }
        Ok(())
    }
}

// ----------------------------------------------------------------- check

/// The read-only inspection the limited child runs before it is `Healthy`.
#[derive(Debug, Clone)]
pub struct StateCheck {
    coordination: Coordination,
    transaction: TransactionId,
    root: PathBuf,
}

impl StateCheck {
    pub(crate) fn new(coordination: Coordination, transaction: TransactionId, root: PathBuf) -> Self {
        Self {
            coordination,
            transaction,
            root,
        }
    }

    /// Reads only. Fails closed on anything that is missing, unreadable,
    /// malformed or bound to something else.
    pub fn verify(&self) -> Result<()> {
        let root = validate_root(&self.root)?;
        // The root the helper bound must still be the root this child holds.
        if read_binding(&self.coordination, &self.transaction)? != root {
            return Err(mismatch("the state root changed after it was bound"));
        }
        // The directory must be listable and the settings store must open.
        fs::read_dir(&root)?;
        let settings = root.join(SETTINGS_FILE);
        match fs::symlink_metadata(&settings) {
            Ok(meta) if meta.is_file() => {
                read_limited(&settings, SETTINGS_LIMIT)?;
            }
            Ok(_) => {
                return Err(UpdateError::new(
                    ErrorCode::StateIncompatible,
                    "the settings store is not a plain file",
                ));
            }
            // A first run has no settings yet; the defaults apply.
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => return Err(err.into()),
        }
        let ticket_path = self.coordination.transaction_dir(&self.transaction).join(TICKET_FILE);
        let bytes = read_private(&ticket_path, TICKET_LIMIT, "the restart ticket")?;
        let ticket: RestartTicket = parse_strict(&bytes)?;
        ticket.validate(self.coordination.install_id(), &self.transaction, &root)
    }
}

// ----------------------------------------------------------------- issue

fn ticket_path(coordination: &Coordination, transaction: &TransactionId) -> PathBuf {
    coordination.transaction_dir(transaction).join(TICKET_FILE)
}

fn claimed_path(coordination: &Coordination, transaction: &TransactionId) -> PathBuf {
    coordination.transaction_dir(transaction).join(CLAIMED_FILE)
}

/// What the application contributes to a ticket. Paths and bounded positions
/// only; every other field of the ticket is derived here.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TicketContent {
    pub work_folder: Option<String>,
    pub files: Vec<TicketFile>,
    /// Index into `files` of the file that was active.
    pub active: Option<u32>,
    /// The enabled Explorer integration, when there is one.
    pub explorer: Option<ExplorerTicket>,
}

/// Parent side: writes `restart-ticket.json` into the transaction directory.
///
/// The ticket is built and fully validated first (schema, size, fields, the
/// bound state root, this installation and transaction). Only then is
/// `still_permitted` asked, as the very last check before the write: it
/// re-confirms the candidate, the placement and the freshness of the restart
/// permit. A refusal leaves no ticket behind. The file is written atomically
/// and flushed before this returns `Ok`.
pub fn issue_ticket(
    coordination: &Coordination,
    transaction: &TransactionId,
    root: &Path,
    content: TicketContent,
    still_permitted: impl FnOnce() -> Result<()>,
) -> Result<()> {
    let root = validate_root(root)?;
    if read_binding(coordination, transaction)? != root {
        return Err(mismatch("the state root is not the one bound to this transaction"));
    }
    let state_root = root
        .to_str()
        .ok_or_else(|| mismatch("the state root is not valid text"))?
        .to_owned();
    let ticket = RestartTicket {
        schema: TICKET_SCHEMA,
        install_id: coordination.install_id().as_str().to_owned(),
        transaction_id: transaction.as_str().to_owned(),
        state_root,
        work_folder: content.work_folder,
        files: content.files,
        active: content.active,
        explorer: content.explorer,
    };
    ticket.validate(coordination.install_id(), transaction, &root)?;
    let bytes = serde_json::to_vec(&ticket)
        .map_err(|err| UpdateError::new(ErrorCode::Io, err.to_string()))?;
    if bytes.len() as u64 > TICKET_LIMIT {
        return Err(UpdateError::new(
            ErrorCode::LimitExceeded,
            "the restart ticket is too large",
        ));
    }
    still_permitted()?;
    let path = ticket_path(coordination, transaction);
    let _ = fs::remove_file(&path);
    durable_write(&path, &bytes)?;
    if let Err(err) = acl::secure_new(&path, false, 0o600).and_then(|()| acl::verify(&path, false)) {
        let _ = fs::remove_file(&path);
        return Err(err);
    }
    Ok(())
}

/// Removes a ticket that is no longer wanted (an aborted restart).
pub fn discard_ticket(coordination: &Coordination, transaction: &TransactionId) {
    let _ = fs::remove_file(ticket_path(coordination, transaction));
}

// ---------------------------------------------------------------- restore

/// Whether the claimed ticket could be removed after it was used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Cleanup {
    Done,
    /// The leftover could not be removed yet. It is harmless (it only marks
    /// the ticket as used) and removal can be retried.
    CleanupPending(String),
}

/// New-version side: processes the ticket of the transaction that just
/// committed, once.
#[derive(Debug, Clone)]
pub struct TicketRestore {
    coordination: Coordination,
    transaction: TransactionId,
    root: PathBuf,
}

impl TicketRestore {
    pub(crate) fn new(coordination: Coordination, transaction: TransactionId, root: PathBuf) -> Self {
        Self {
            coordination,
            transaction,
            root,
        }
    }

    /// Normal-startup side: the restore of `transaction` bound to the root its
    /// binding names. A missing, foreign or invalid binding is an error, so a
    /// restore is never built for another install, transaction or root.
    pub(crate) fn from_binding(coordination: &Coordination, transaction: &TransactionId) -> Result<Self> {
        let root = read_binding(coordination, transaction)?;
        Ok(Self::new(coordination.clone(), transaction.clone(), root))
    }

    /// The bound state root; [`Self::take`] re-validates it against the binding.
    pub fn state_root(&self) -> &Path {
        &self.root
    }

    /// Whether an unclaimed ticket is still waiting. Reads nothing and changes
    /// nothing.
    pub fn has_unclaimed_ticket(&self) -> bool {
        fs::symlink_metadata(claimed_path(&self.coordination, &self.transaction)).is_err()
            && fs::symlink_metadata(ticket_path(&self.coordination, &self.transaction)).is_ok()
    }

    fn committed_durably(&self) -> bool {
        let store = FileJournalStore::new(self.coordination.journal_path(&self.transaction));
        matches!(
            store.load(),
            Ok(Some(journal))
                if journal.phase == Phase::Committed
                    && journal.transaction_id == self.transaction
                    && &journal.install_id == self.coordination.install_id()
        )
    }

    /// Takes the ticket exactly once. It refuses unless the journal of this
    /// transaction is durably `Committed`. The ticket is renamed to a claimed
    /// file before it is read, so a crash while restoring, a second start or a
    /// damaged ticket can never make the same ticket apply twice. `Ok(None)`
    /// means there was nothing (left) to restore.
    pub fn take(&self) -> Result<Option<RestartTicket>> {
        self.take_with(sync_renamed_file)
    }

    /// [`Self::take`] with the durability barrier of the claim rename as a
    /// seam. The ticket is returned only after `barrier` confirmed the claim
    /// is durable. If it fails, the rename is undone (best effort) and the
    /// error is returned, so the ticket is neither handed out nor lost to a
    /// rename that may or may not survive a power loss.
    pub(crate) fn take_with(
        &self,
        barrier: impl FnOnce(&Path) -> std::io::Result<()>,
    ) -> Result<Option<RestartTicket>> {
        if !self.committed_durably() {
            return Err(UpdateError::new(
                ErrorCode::NotApplicable,
                "the update is not durably committed, so its ticket is not used",
            ));
        }
        let root = validate_root(&self.root)?;
        if read_binding(&self.coordination, &self.transaction)? != root {
            return Err(mismatch("the state root changed after it was bound"));
        }
        let claimed = claimed_path(&self.coordination, &self.transaction);
        if fs::symlink_metadata(&claimed).is_ok() {
            return Ok(None);
        }
        let ticket = ticket_path(&self.coordination, &self.transaction);
        // Write-through and never replacing: the claim is durable when it
        // returns, and only one claimer can win the claimed name.
        match rename_new_durable(&ticket, &claimed) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(err) => return Err(err.into()),
        }
        if let Err(err) = barrier(&claimed) {
            // Not durable: do not treat the claim as made.
            let _ = fs::rename(&claimed, &ticket);
            return Err(err.into());
        }
        let bytes = read_private(&claimed, TICKET_LIMIT, "the claimed restart ticket")?;
        let ticket: RestartTicket = parse_strict(&bytes)?;
        ticket.validate(self.coordination.install_id(), &self.transaction, &root)?;
        Ok(Some(ticket))
    }

    /// Removes the claimed ticket. A failure is reported as pending, not as an
    /// error: the files were already restored and nothing is repeated.
    pub fn finish(&self) -> Cleanup {
        self.finish_with(|path| fs::remove_file(path))
    }

    pub(crate) fn finish_with(&self, remove: impl FnOnce(&Path) -> std::io::Result<()>) -> Cleanup {
        let claimed = claimed_path(&self.coordination, &self.transaction);
        match remove(&claimed) {
            Ok(()) => Cleanup::Done,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Cleanup::Done,
            Err(err) => Cleanup::CleanupPending(crate::protocol::bounded_reason(&err.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::InstallFacts;
    use crate::testing::TempDir;

    fn install_id() -> InstallId {
        InstallId::derive(&InstallFacts {
            os: "test",
            volume_id: "v",
            install_path: "/x",
            owner: "u",
        })
        .unwrap()
    }

    struct Fixture {
        dir: TempDir,
        coordination: Coordination,
        transaction: TransactionId,
        root: PathBuf,
    }

    fn private_dir(path: &Path) {
        fs::create_dir_all(path).unwrap();
        acl::secure_new(path, true, 0o700).unwrap();
    }

    fn fixture() -> Fixture {
        let dir = TempDir::new("state-root");
        let coordination = Coordination::create(dir.path(), &install_id()).unwrap();
        let transaction = TransactionId::generate().unwrap();
        coordination.ensure_helper_dir(&transaction).unwrap();
        let root = dir.path().join("state");
        private_dir(&root);
        Fixture {
            dir,
            coordination,
            transaction,
            root,
        }
    }

    impl Fixture {
        fn check(&self) -> StateCheck {
            StateCheck::new(
                self.coordination.clone(),
                self.transaction.clone(),
                self.root.clone(),
            )
        }

        fn ticket(&self) -> RestartTicket {
            RestartTicket {
                schema: TICKET_SCHEMA,
                install_id: install_id().as_str().to_owned(),
                transaction_id: self.transaction.as_str().to_owned(),
                state_root: self.root.to_str().unwrap().to_owned(),
                work_folder: Some(self.dir.path().join("work").to_str().unwrap().to_owned()),
                files: vec![TicketFile {
                    path: self.dir.path().join("work/a.md").to_str().unwrap().to_owned(),
                    scroll_line: 3,
                    caret_offset: 10,
                }],
                active: Some(0),
                explorer: None,
            }
        }

        fn write_private(&self, name: &str, bytes: &[u8]) {
            let path = self.coordination.transaction_dir(&self.transaction).join(name);
            durable_write(&path, bytes).unwrap();
            acl::secure_new(&path, false, 0o600).unwrap();
        }

        fn write_ticket(&self, ticket: &RestartTicket) {
            self.write_private(TICKET_FILE, &serde_json::to_vec(ticket).unwrap());
        }

        /// A bound root and a valid ticket.
        fn ready(&self) {
            publish_binding(&self.coordination, &self.transaction, &self.root).unwrap();
            self.write_ticket(&self.ticket());
        }

        fn snapshot(&self) -> Vec<(String, Vec<u8>)> {
            let mut seen = Vec::new();
            for base in [self.root.clone(), self.coordination.transaction_dir(&self.transaction)] {
                let mut entries: Vec<_> = fs::read_dir(&base).unwrap().flatten().collect();
                entries.sort_by_key(fs::DirEntry::file_name);
                for entry in entries {
                    let path = entry.path();
                    let bytes = if path.is_file() { fs::read(&path).unwrap() } else { Vec::new() };
                    seen.push((path.display().to_string(), bytes));
                }
            }
            seen
        }
    }

    #[test]
    fn only_an_absolute_normalized_owned_directory_is_a_root() {
        let fx = fixture();
        assert_eq!(validate_root(&fx.root).unwrap(), fx.root);

        let text = fx.root.to_str().unwrap();
        let sep = std::path::MAIN_SEPARATOR;
        for bad in [
            PathBuf::new(),
            PathBuf::from("state"),
            PathBuf::from(format!("{text}{sep}..{sep}state")),
            PathBuf::from(format!("{text}{sep}.")),
            PathBuf::from(format!("{text}{sep}")),
            PathBuf::from(format!("{text}{sep}{sep}sub")),
            fx.dir.path().join("missing"),
            PathBuf::from(format!("{text}\n")),
        ] {
            assert!(validate_root(&bad).is_err(), "{bad:?}");
        }

        // A file is not a root.
        let file = fx.dir.path().join("file");
        fs::write(&file, b"x").unwrap();
        assert!(validate_root(&file).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn a_linked_or_shared_root_is_refused() {
        use std::os::unix::fs::{PermissionsExt, symlink};

        let fx = fixture();
        let link = fx.dir.path().join("link");
        symlink(&fx.root, &link).unwrap();
        assert!(validate_root(&link).is_err());

        let shared = fx.dir.path().join("shared");
        private_dir(&shared);
        fs::set_permissions(&shared, fs::Permissions::from_mode(0o777)).unwrap();
        assert!(validate_root(&shared).is_err());
    }

    #[test]
    fn the_binding_carries_the_root_to_the_helper_and_only_for_its_transaction() {
        let fx = fixture();
        // Absent.
        assert!(read_binding(&fx.coordination, &fx.transaction).is_err());

        publish_binding(&fx.coordination, &fx.transaction, &fx.root).unwrap();
        assert_eq!(read_binding(&fx.coordination, &fx.transaction).unwrap(), fx.root);

        // Another transaction never sees it.
        let other = TransactionId::generate().unwrap();
        fx.coordination.ensure_helper_dir(&other).unwrap();
        assert!(read_binding(&fx.coordination, &other).is_err());

        // A root that is not valid is not published.
        let relative = Path::new("relative");
        assert!(publish_binding(&fx.coordination, &other, relative).is_err());
        assert!(read_binding(&fx.coordination, &other).is_err());

        // Discarded.
        discard_binding(&fx.coordination, &fx.transaction);
        assert!(read_binding(&fx.coordination, &fx.transaction).is_err());
    }

    #[test]
    fn only_allowlisted_variables_travel_to_the_child() {
        let fx = fixture();
        // Nothing recorded: no variables.
        assert!(read_environment(&fx.coordination, &fx.transaction).unwrap().is_empty());

        let ambient = [
            ("HOME".to_owned(), "/home/u".to_owned()),
            ("LOCALAPPDATA".to_owned(), "C:\\Users\\u\\AppData\\Local".to_owned()),
            ("TMP".to_owned(), "bad\nvalue".to_owned()),
            ("TEMP".to_owned(), "x".repeat(MAX_ENV_VALUE_BYTES + 1)),
        ]
        .map(|(k, v)| (std::ffi::OsString::from(k), std::ffi::OsString::from(v)));
        publish_environment(&fx.coordination, &fx.transaction, ambient).unwrap();
        let vars = read_environment(&fx.coordination, &fx.transaction).unwrap();
        assert_eq!(
            vars,
            [
                ("HOME".to_owned(), "/home/u".to_owned()),
                ("LOCALAPPDATA".to_owned(), "C:\\Users\\u\\AppData\\Local".to_owned()),
            ]
        );

        // A loader variable is never accepted, even if it is written by hand.
        let install = install_id();
        for name in ["LD_PRELOAD", "DYLD_INSERT_LIBRARIES", "HANE_COMMAND"] {
            let doc = serde_json::json!({
                "schema": 1,
                "install_id": install.as_str(),
                "transaction_id": fx.transaction.as_str(),
                "vars": { name: "/tmp/evil" },
            });
            fx.write_private(ENVIRONMENT_FILE, &serde_json::to_vec(&doc).unwrap());
            assert!(read_environment(&fx.coordination, &fx.transaction).is_err(), "{name}");
        }

        // Another transaction's record is refused.
        let doc = serde_json::json!({
            "schema": 1,
            "install_id": install.as_str(),
            "transaction_id": TransactionId::generate().unwrap().as_str(),
            "vars": { "HOME": "/home/u" },
        });
        fx.write_private(ENVIRONMENT_FILE, &serde_json::to_vec(&doc).unwrap());
        assert!(read_environment(&fx.coordination, &fx.transaction).is_err());

        // Oversized and damaged records are refused.
        fx.write_private(ENVIRONMENT_FILE, &vec![b' '; 20_000]);
        assert!(read_environment(&fx.coordination, &fx.transaction).is_err());
        fx.write_private(ENVIRONMENT_FILE, b"not json");
        assert!(read_environment(&fx.coordination, &fx.transaction).is_err());
    }

    #[test]
    fn a_long_path_is_kept_but_other_values_and_the_whole_record_stay_bounded() {
        let fx = fixture();
        let long_path = format!("C:\\{}", "a;".repeat(2048));
        assert!(long_path.len() > MAX_ENV_VALUE_BYTES && long_path.len() <= 8 * 1024);
        let too_long_path = "p".repeat(8 * 1024 + 1);
        let to_os = |(k, v): (&str, String)| (std::ffi::OsString::from(k), std::ffi::OsString::from(v));

        // A PATH above 1 KiB and within the PATH limit is recorded.
        publish_environment(
            &fx.coordination,
            &fx.transaction,
            [("PATH", long_path.clone()), ("HOME", "/home/u".to_owned())].map(to_os),
        )
        .unwrap();
        let vars = read_environment(&fx.coordination, &fx.transaction).unwrap();
        assert!(vars.contains(&("PATH".to_owned(), long_path.clone())));

        // A PATH over its own limit is left out, and other variables keep 1 KiB.
        publish_environment(
            &fx.coordination,
            &fx.transaction,
            [
                ("PATH", too_long_path.clone()),
                ("TEMP", "x".repeat(MAX_ENV_VALUE_BYTES + 1)),
                ("HOME", "/home/u".to_owned()),
            ]
            .map(to_os),
        )
        .unwrap();
        assert_eq!(
            read_environment(&fx.coordination, &fx.transaction).unwrap(),
            [("HOME".to_owned(), "/home/u".to_owned())]
        );

        // A hand-written record is checked with the same per-key limits.
        let install = install_id();
        for (name, value) in [
            ("PATH", too_long_path),
            ("TEMP", "x".repeat(MAX_ENV_VALUE_BYTES + 1)),
        ] {
            let doc = serde_json::json!({
                "schema": 1,
                "install_id": install.as_str(),
                "transaction_id": fx.transaction.as_str(),
                "vars": { name: value },
            });
            fx.write_private(ENVIRONMENT_FILE, &serde_json::to_vec(&doc).unwrap());
            assert!(read_environment(&fx.coordination, &fx.transaction).is_err(), "{name}");
        }

        // The whole-record limit is unchanged.
        assert_eq!(ENVIRONMENT_LIMIT, 16 * 1024);
        fx.write_private(ENVIRONMENT_FILE, &vec![b' '; ENVIRONMENT_LIMIT as usize + 1]);
        assert!(read_environment(&fx.coordination, &fx.transaction).is_err());
    }

    #[test]
    fn a_record_over_the_whole_limit_is_refused_before_it_is_written() {
        let fx = fixture();
        let to_os = |(k, v): (&str, String)| (std::ffi::OsString::from(k), std::ffi::OsString::from(v));

        // Every value is within its own per-key limit, but together they
        // serialize past ENVIRONMENT_LIMIT.
        let oversized = [
            ("PATH", "p".repeat(MAX_PATH_ENV_VALUE_BYTES)),
            ("HOME", "h".repeat(MAX_ENV_VALUE_BYTES)),
            ("TMPDIR", "t".repeat(MAX_ENV_VALUE_BYTES)),
            ("TEMP", "e".repeat(MAX_ENV_VALUE_BYTES)),
            ("TMP", "m".repeat(MAX_ENV_VALUE_BYTES)),
            ("APPDATA", "a".repeat(MAX_ENV_VALUE_BYTES)),
            ("LOCALAPPDATA", "l".repeat(MAX_ENV_VALUE_BYTES)),
            ("USERPROFILE", "u".repeat(MAX_ENV_VALUE_BYTES)),
            ("XDG_CONFIG_HOME", "c".repeat(MAX_ENV_VALUE_BYTES)),
            ("XDG_DATA_HOME", "d".repeat(MAX_ENV_VALUE_BYTES)),
        ];
        let total: usize = oversized.iter().map(|(_, v)| v.len()).sum();
        assert!(total > ENVIRONMENT_LIMIT as usize);

        // A previously published, valid record must not survive as a stale one.
        publish_environment(
            &fx.coordination,
            &fx.transaction,
            [("HOME", "/home/u".to_owned())].map(to_os),
        )
        .unwrap();
        let err = publish_environment(&fx.coordination, &fx.transaction, oversized.map(to_os))
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::LimitExceeded);
        assert!(!environment_path(&fx.coordination, &fx.transaction).exists());
        assert!(read_environment(&fx.coordination, &fx.transaction).unwrap().is_empty());

        // A record within the limit is still published and read back.
        let within = [
            ("PATH", "p".repeat(MAX_PATH_ENV_VALUE_BYTES)),
            ("HOME", "h".repeat(MAX_ENV_VALUE_BYTES)),
        ];
        publish_environment(&fx.coordination, &fx.transaction, within.clone().map(to_os)).unwrap();
        let vars = read_environment(&fx.coordination, &fx.transaction).unwrap();
        for (name, value) in within {
            assert!(vars.contains(&(name.to_owned(), value)));
        }
    }

    #[test]
    fn program_files_locations_are_stored_and_restored() {
        let fx = fixture();
        let program_files = [
            ("ProgramFiles", "C:\\Program Files"),
            ("ProgramFiles(x86)", "C:\\Program Files (x86)"),
        ];
        let ambient: Vec<(std::ffi::OsString, std::ffi::OsString)> = program_files
            .iter()
            .map(|(key, value)| ((*key).into(), (*value).into()))
            .collect();
        publish_environment(&fx.coordination, &fx.transaction, ambient).unwrap();
        let vars = read_environment(&fx.coordination, &fx.transaction).unwrap();
        for (name, value) in program_files {
            assert!(
                vars.contains(&(name.to_owned(), value.to_owned())),
                "{name} must reach the limited child"
            );
        }
    }

    #[test]
    fn network_settings_reach_the_child_but_loader_and_session_variables_do_not() {
        let fx = fixture();
        let network = [
            ("ALL_PROXY", "socks5://proxy.example:1080"),
            ("HTTP_PROXY", "http://proxy.example:8080"),
            ("HTTPS_PROXY", "http://proxy.example:8443"),
            ("NO_PROXY", "localhost,127.0.0.1"),
            ("CURL_CA_BUNDLE", "/etc/ssl/ca.pem"),
            ("REQUESTS_CA_BUNDLE", "/etc/ssl/ca.pem"),
            ("SSL_CERT_DIR", "/etc/ssl/certs"),
            ("SSL_CERT_FILE", "/etc/ssl/ca.pem"),
        ];
        let forbidden = ["LD_PRELOAD", "DYLD_INSERT_LIBRARIES", "CODEX_HOME", "HANE_COMMAND"];
        let ambient: Vec<(std::ffi::OsString, std::ffi::OsString)> = network
            .iter()
            .map(|(key, value)| ((*key).into(), (*value).into()))
            .chain(forbidden.iter().map(|key| ((*key).into(), "/tmp/evil".into())))
            .collect();
        publish_environment(&fx.coordination, &fx.transaction, ambient).unwrap();
        let vars = read_environment(&fx.coordination, &fx.transaction).unwrap();
        for (name, value) in network {
            assert!(
                vars.contains(&(name.to_owned(), value.to_owned())),
                "{name} must reach the limited child"
            );
        }
        for name in forbidden {
            assert!(vars.iter().all(|(key, _)| key != name), "{name} must stay out");
        }
        assert_eq!(vars.len(), network.len());
    }

    struct MemorySecrets {
        entries: std::cell::RefCell<std::collections::BTreeMap<String, String>>,
        capacity: usize,
    }

    impl MemorySecrets {
        fn new(capacity: usize) -> Self {
            Self { entries: Default::default(), capacity }
        }

        fn len(&self) -> usize {
            self.entries.borrow().len()
        }
    }

    impl TransientSecretStore for MemorySecrets {
        fn put(&self, reference: &str, secret: &str) -> Result<()> {
            if self.entries.borrow().len() >= self.capacity {
                return Err(secret_store_unavailable());
            }
            self.entries.borrow_mut().insert(reference.to_owned(), secret.to_owned());
            Ok(())
        }

        fn take(&self, reference: &str) -> Result<Option<String>> {
            Ok(self.entries.borrow_mut().remove(reference))
        }

        fn delete(&self, reference: &str) -> Result<()> {
            self.entries.borrow_mut().remove(reference);
            Ok(())
        }
    }

    const DUMMY_USER: &str = "dummy-proxy-user";
    const DUMMY_PASS: &str = "dummy-proxy-pass";

    fn proxy_ambient() -> Vec<(std::ffi::OsString, std::ffi::OsString)> {
        [
            ("HTTPS_PROXY", format!("http://{DUMMY_USER}:{DUMMY_PASS}@proxy.example:8443")),
            ("http_proxy", "http://plain.example:8080".to_owned()),
            ("HOME", "/home/u".to_owned()),
        ]
        .into_iter()
        .map(|(k, v)| (std::ffi::OsString::from(k), std::ffi::OsString::from(v)))
        .collect()
    }

    fn recorded_environment(fx: &Fixture) -> Option<Vec<u8>> {
        fs::read(fx.coordination.transaction_dir(&fx.transaction).join(ENVIRONMENT_FILE)).ok()
    }

    #[test]
    fn proxy_userinfo_is_never_written_and_without_a_secret_store_the_handoff_aborts() {
        let fx = fixture();
        let err = publish_environment(&fx.coordination, &fx.transaction, proxy_ambient());
        assert!(err.is_err());
        assert!(recorded_environment(&fx).is_none());
        assert!(!format!("{err:?}").contains(DUMMY_PASS));
    }

    #[test]
    fn proxy_userinfo_travels_through_a_one_time_transaction_bound_secret() {
        let fx = fixture();
        let secrets = MemorySecrets::new(8);
        publish_environment_with_secrets(&fx.coordination, &fx.transaction, proxy_ambient(), &secrets)
            .unwrap();
        assert_eq!(secrets.len(), 1);

        let text = String::from_utf8(recorded_environment(&fx).unwrap()).unwrap();
        assert!(!text.contains(DUMMY_USER) && !text.contains(DUMMY_PASS));
        assert!(text.contains("proxy.example:8443"));
        assert!(text.contains(fx.transaction.as_str()));

        // Without the store the record is refused rather than restored bare.
        assert!(read_environment(&fx.coordination, &fx.transaction).is_err());
        assert_eq!(secrets.len(), 1);

        let vars =
            read_environment_with_secrets(&fx.coordination, &fx.transaction, &secrets).unwrap();
        assert!(vars.contains(&(
            "HTTPS_PROXY".to_owned(),
            format!("http://{DUMMY_USER}:{DUMMY_PASS}@proxy.example:8443")
        )));
        assert!(vars.contains(&("http_proxy".to_owned(), "http://plain.example:8080".to_owned())));
        // Consumed exactly once.
        assert_eq!(secrets.len(), 0);
        assert!(read_environment_with_secrets(&fx.coordination, &fx.transaction, &secrets).is_err());
    }

    #[test]
    fn a_failed_secret_store_leaves_no_record_and_no_secret() {
        let fx = fixture();
        let ambient: Vec<(std::ffi::OsString, std::ffi::OsString)> = [
            ("HTTPS_PROXY", format!("http://{DUMMY_USER}:{DUMMY_PASS}@a.example:1")),
            ("HTTP_PROXY", format!("http://{DUMMY_USER}:{DUMMY_PASS}@b.example:2")),
        ]
        .into_iter()
        .map(|(k, v)| (std::ffi::OsString::from(k), std::ffi::OsString::from(v)))
        .collect();
        let secrets = MemorySecrets::new(1);
        assert!(
            publish_environment_with_secrets(&fx.coordination, &fx.transaction, ambient, &secrets)
                .is_err()
        );
        assert_eq!(secrets.len(), 0);
        assert!(recorded_environment(&fx).is_none());
    }

    #[test]
    fn every_terminal_path_discards_the_secret_and_the_record() {
        let fx = fixture();
        let secrets = MemorySecrets::new(8);
        publish_environment_with_secrets(&fx.coordination, &fx.transaction, proxy_ambient(), &secrets)
            .unwrap();
        release_environment_secrets(&fx.coordination, &fx.transaction, &secrets).unwrap();
        assert_eq!(secrets.len(), 0);
        assert!(recorded_environment(&fx).is_none());
        // Idempotent.
        release_environment_secrets(&fx.coordination, &fx.transaction, &secrets).unwrap();
    }

    /// A store whose `delete` can be made to fail, wrapping the memory store.
    struct FlakyDelete {
        inner: MemorySecrets,
        fail_delete: std::cell::Cell<bool>,
        fail_take: bool,
    }

    impl FlakyDelete {
        fn new() -> Self {
            Self {
                inner: MemorySecrets::new(8),
                fail_delete: std::cell::Cell::new(false),
                fail_take: false,
            }
        }
    }

    impl TransientSecretStore for FlakyDelete {
        fn put(&self, reference: &str, secret: &str) -> Result<()> {
            self.inner.put(reference, secret)
        }

        fn take(&self, reference: &str) -> Result<Option<String>> {
            if self.fail_take {
                return Err(secret_store_unavailable());
            }
            self.inner.take(reference)
        }

        fn delete(&self, reference: &str) -> Result<()> {
            if self.fail_delete.get() {
                return Err(secret_store_unavailable());
            }
            self.inner.delete(reference)
        }
    }

    fn two_credentials() -> Vec<(std::ffi::OsString, std::ffi::OsString)> {
        [
            ("HTTPS_PROXY", format!("http://{DUMMY_USER}:{DUMMY_PASS}@a.example:1")),
            ("HTTP_PROXY", format!("http://{DUMMY_USER}:{DUMMY_PASS}@b.example:2")),
        ]
        .into_iter()
        .map(|(k, v)| (std::ffi::OsString::from(k), std::ffi::OsString::from(v)))
        .collect()
    }

    #[test]
    fn a_failed_secret_delete_keeps_the_record_and_a_retry_cleans_up() {
        let fx = fixture();
        let secrets = FlakyDelete::new();
        publish_environment_with_secrets(&fx.coordination, &fx.transaction, two_credentials(), &secrets)
            .unwrap();
        assert_eq!(secrets.inner.len(), 2);

        secrets.fail_delete.set(true);
        assert!(release_environment_secrets(&fx.coordination, &fx.transaction, &secrets).is_err());
        // Still tracked: the record names the references and the secrets remain.
        assert!(recorded_environment(&fx).is_some());
        assert_eq!(secrets.inner.len(), 2);

        secrets.fail_delete.set(false);
        release_environment_secrets(&fx.coordination, &fx.transaction, &secrets).unwrap();
        assert_eq!(secrets.inner.len(), 0);
        assert!(recorded_environment(&fx).is_none());
    }

    #[test]
    fn a_secret_whose_record_was_never_written_is_still_released() {
        let fx = fixture();
        let secrets = FlakyDelete::new();
        // A partly published handoff: the secret exists, no record names it.
        secrets.put(&secret_ref(&fx.transaction, 1), DUMMY_PASS).unwrap();
        assert!(recorded_environment(&fx).is_none());
        release_environment_secrets(&fx.coordination, &fx.transaction, &secrets).unwrap();
        assert_eq!(secrets.inner.len(), 0);
    }

    #[test]
    fn publishing_again_replaces_the_secrets_of_the_first_publish() {
        let fx = fixture();
        let secrets = MemorySecrets::new(8);
        publish_environment_with_secrets(&fx.coordination, &fx.transaction, two_credentials(), &secrets)
            .unwrap();
        release_environment_secrets(&fx.coordination, &fx.transaction, &secrets).unwrap();
        publish_environment_with_secrets(&fx.coordination, &fx.transaction, proxy_ambient(), &secrets)
            .unwrap();
        assert_eq!(secrets.len(), 1);
    }

    #[test]
    fn a_read_failure_never_yields_an_unauthenticated_proxy_and_stays_releasable() {
        let fx = fixture();
        let mut secrets = FlakyDelete::new();
        publish_environment_with_secrets(&fx.coordination, &fx.transaction, proxy_ambient(), &secrets)
            .unwrap();
        secrets.fail_take = true;
        assert!(read_environment_with_secrets(&fx.coordination, &fx.transaction, &secrets).is_err());
        // The secret was not consumed and is still tracked for the cleanup.
        assert_eq!(secrets.inner.len(), 1);
        assert!(recorded_environment(&fx).is_some());
        release_environment_secrets(&fx.coordination, &fx.transaction, &secrets).unwrap();
        assert_eq!(secrets.inner.len(), 0);
    }

    #[test]
    fn more_credentials_than_the_bound_abort_and_leave_nothing() {
        let fx = fixture();
        let secrets = MemorySecrets::new(64);
        let names = [
            "ALL_PROXY", "all_proxy", "All_Proxy", "HTTP_PROXY", "http_proxy", "Http_Proxy",
            "HTTPS_PROXY", "https_proxy", "Https_Proxy",
        ];
        let ambient: Vec<(std::ffi::OsString, std::ffi::OsString)> = names
            .iter()
            .map(|name| {
                (
                    std::ffi::OsString::from(*name),
                    std::ffi::OsString::from(format!("http://{DUMMY_USER}:{DUMMY_PASS}@p.example:1")),
                )
            })
            .collect();
        assert!(
            publish_environment_with_secrets(&fx.coordination, &fx.transaction, ambient, &secrets)
                .is_err()
        );
        assert_eq!(secrets.len(), 0);
        assert!(recorded_environment(&fx).is_none());
    }

    #[test]
    fn no_secret_reaches_the_record_or_an_error_when_a_later_credential_fails() {
        let fx = fixture();
        let secrets = MemorySecrets::new(1);
        let err = publish_environment_with_secrets(&fx.coordination, &fx.transaction, two_credentials(), &secrets)
            .unwrap_err();
        assert!(!format!("{err:?} {err}").contains(DUMMY_PASS));
        assert_eq!(secrets.len(), 0);
        assert!(recorded_environment(&fx).is_none());
    }

    #[test]
    fn a_missing_secret_is_an_error_and_the_other_secrets_are_still_consumed() {
        let fx = fixture();
        let secrets = MemorySecrets::new(8);
        let ambient: Vec<(std::ffi::OsString, std::ffi::OsString)> = [
            ("HTTPS_PROXY", format!("http://{DUMMY_USER}:{DUMMY_PASS}@a.example:1")),
            ("HTTP_PROXY", format!("http://{DUMMY_USER}:{DUMMY_PASS}@b.example:2")),
        ]
        .into_iter()
        .map(|(k, v)| (std::ffi::OsString::from(k), std::ffi::OsString::from(v)))
        .collect();
        publish_environment_with_secrets(&fx.coordination, &fx.transaction, ambient, &secrets)
            .unwrap();
        assert_eq!(secrets.len(), 2);
        let first = secret_ref(&fx.transaction, 0);
        secrets.delete(&first).unwrap();
        assert!(
            read_environment_with_secrets(&fx.coordination, &fx.transaction, &secrets).is_err()
        );
        assert_eq!(secrets.len(), 0);
    }

    #[test]
    fn a_forged_or_foreign_secret_reference_and_inline_credentials_are_refused() {
        let fx = fixture();
        let install = install_id();
        let other = TransactionId::generate().unwrap();
        for (vars, refs) in [
            (
                serde_json::json!({ "HTTPS_PROXY": "http://proxy.example:1" }),
                serde_json::json!({ "HTTPS_PROXY": secret_ref(&other, 0) }),
            ),
            (
                serde_json::json!({ "HTTPS_PROXY": "http://proxy.example:1" }),
                serde_json::json!({ "NO_PROXY": secret_ref(&fx.transaction, 0) }),
            ),
            (
                serde_json::json!({ "HTTPS_PROXY": "http://proxy.example:1" }),
                serde_json::json!({ "HTTPS_PROXY": "../escape" }),
            ),
            (
                serde_json::json!({ "HTTPS_PROXY": "http://a:b@proxy.example:1" }),
                serde_json::json!({}),
            ),
        ] {
            let doc = serde_json::json!({
                "schema": 1,
                "install_id": install.as_str(),
                "transaction_id": fx.transaction.as_str(),
                "vars": vars,
                "secret_refs": refs,
            });
            fx.write_private(ENVIRONMENT_FILE, &serde_json::to_vec(&doc).unwrap());
            assert!(
                read_environment_with_secrets(&fx.coordination, &fx.transaction, &MemorySecrets::new(1))
                    .is_err()
            );
        }
    }

    #[test]
    fn credentials_outside_the_authority_are_not_stored() {
        for value in ["http://proxy.example/path?token=a@b", "proxy.example:1/#x@y"] {
            assert!(split_userinfo(value).is_err(), "{value}");
        }
        assert_eq!(
            split_userinfo("user:pw@proxy.example:1").unwrap(),
            ("proxy.example:1".to_owned(), Some("user:pw".to_owned()))
        );
        assert_eq!(join_userinfo("proxy.example:1", "user:pw"), "user:pw@proxy.example:1");
    }

    #[test]
    fn lowercase_network_variables_are_kept_and_unlisted_case_variants_are_not() {
        let fx = fixture();
        let kept = [
            ("https_proxy", "http://proxy.example:8443"),
            ("no_proxy", "localhost"),
            ("Ssl_Cert_File", "/etc/ssl/ca.pem"),
        ];
        let dropped = ["ld_preload", "dyld_insert_libraries"];
        let ambient: Vec<(std::ffi::OsString, std::ffi::OsString)> = kept
            .iter()
            .map(|(key, value)| ((*key).into(), (*value).into()))
            .chain(dropped.iter().map(|key| ((*key).into(), "/tmp/evil".into())))
            .collect();
        publish_environment(&fx.coordination, &fx.transaction, ambient).unwrap();
        let vars = read_environment(&fx.coordination, &fx.transaction).unwrap();
        for (name, value) in kept {
            assert!(
                vars.contains(&(name.to_owned(), value.to_owned())),
                "{name} must be restored with its original spelling"
            );
        }
        assert_eq!(vars.len(), kept.len());
    }

    #[test]
    fn windows_execution_variables_reach_the_child_and_unlisted_ones_do_not() {
        let fx = fixture();
        let execution = [
            ("Path", "C:\\Windows\\System32;C:\\Tools"),
            ("PATHEXT", ".COM;.EXE;.BAT;.CMD"),
            ("ComSpec", "C:\\Windows\\System32\\cmd.exe"),
            ("WINDIR", "C:\\Windows"),
            ("PROCESSOR_ARCHITECTURE", "AMD64"),
        ];
        let unlisted = ["CODEX_HOME", "OPENAI_API_KEY", "PSModulePath"];
        let ambient: Vec<(std::ffi::OsString, std::ffi::OsString)> = execution
            .iter()
            .map(|(key, value)| ((*key).into(), (*value).into()))
            .chain(unlisted.iter().map(|key| ((*key).into(), "secret".into())))
            .collect();
        publish_environment(&fx.coordination, &fx.transaction, ambient).unwrap();
        let vars = read_environment(&fx.coordination, &fx.transaction).unwrap();
        for (name, value) in execution {
            assert!(
                vars.contains(&(name.to_owned(), value.to_owned())),
                "{name} must reach the limited child"
            );
        }
        for name in unlisted {
            assert!(vars.iter().all(|(key, _)| key != name), "{name} must stay out");
        }
        assert_eq!(vars.len(), execution.len());
    }

    #[test]
    fn the_runtime_dir_and_text_encoding_reach_the_child_and_unlisted_ones_do_not() {
        let fx = fixture();
        let listed = [
            ("XDG_RUNTIME_DIR", "/run/user/1000"),
            ("__CF_USER_TEXT_ENCODING", "0x1F5:0x8000100:0x52"),
        ];
        let unlisted = ["CODEX_HOME", "OPENAI_API_KEY"];
        let ambient: Vec<(std::ffi::OsString, std::ffi::OsString)> = listed
            .iter()
            .map(|(key, value)| ((*key).into(), (*value).into()))
            .chain(unlisted.iter().map(|key| ((*key).into(), "secret".into())))
            .collect();
        publish_environment(&fx.coordination, &fx.transaction, ambient).unwrap();
        let vars = read_environment(&fx.coordination, &fx.transaction).unwrap();
        for (name, value) in listed {
            assert!(
                vars.contains(&(name.to_owned(), value.to_owned())),
                "{name} must reach the limited child"
            );
        }
        for name in unlisted {
            assert!(vars.iter().all(|(key, _)| key != name), "{name} must stay out");
        }
        assert_eq!(vars.len(), listed.len());
    }

    #[cfg(unix)]
    #[test]
    fn an_unreadable_environment_record_is_not_an_empty_one() {
        let fx = fixture();
        // Absent is the only "no variables" case.
        assert!(read_environment(&fx.coordination, &fx.transaction).unwrap().is_empty());

        // Make the record's metadata fail with something other than NotFound:
        // its parent is a plain file, so the lookup ends in ENOTDIR.
        let dir = fx.coordination.transaction_dir(&fx.transaction);
        fs::remove_dir_all(&dir).unwrap();
        fs::write(&dir, b"not a directory").unwrap();
        let err = fs::symlink_metadata(dir.join(ENVIRONMENT_FILE)).unwrap_err();
        assert_ne!(err.kind(), std::io::ErrorKind::NotFound);
        assert!(read_environment(&fx.coordination, &fx.transaction).is_err());
    }

    #[test]
    fn a_forged_or_damaged_binding_is_refused() {
        let fx = fixture();
        let root = fx.root.to_str().unwrap();
        let doc = |schema: u32, install: &str, txn: &str, root: &str| {
            format!(
                "{{\"schema\":{schema},\"install_id\":\"{install}\",\"transaction_id\":\"{txn}\",\"state_root\":{}}}",
                serde_json::to_string(root).unwrap()
            )
        };
        let install = install_id();
        let txn = fx.transaction.as_str();
        let foreign_install = "a".repeat(32);
        let foreign_txn = TransactionId::generate().unwrap();
        // The well-formed document is accepted, so each change below is what is refused.
        fx.write_private(BINDING_FILE, doc(1, install.as_str(), txn, root).as_bytes());
        assert_eq!(read_binding(&fx.coordination, &fx.transaction).unwrap(), fx.root);

        for bad in [
            doc(2, install.as_str(), txn, root),
            doc(1, &foreign_install, txn, root),
            doc(1, install.as_str(), foreign_txn.as_str(), root),
            doc(1, install.as_str(), txn, "relative"),
            doc(1, install.as_str(), txn, &format!("{root}/../state")),
            doc(1, install.as_str(), txn, &format!("{root}-other")),
            format!("{} ", doc(1, install.as_str(), txn, root)).replace('}', ",\"extra\":1}"),
            "not json".to_owned(),
            String::new(),
        ] {
            fx.write_private(BINDING_FILE, bad.as_bytes());
            assert!(read_binding(&fx.coordination, &fx.transaction).is_err(), "{bad}");
        }

        fx.write_private(BINDING_FILE, &vec![b' '; 5000]);
        assert_eq!(
            read_binding(&fx.coordination, &fx.transaction).unwrap_err().code,
            ErrorCode::LimitExceeded
        );
    }

    #[test]
    fn a_bound_root_and_a_valid_ticket_pass_without_changing_anything() {
        let fx = fixture();
        fs::write(fx.root.join(SETTINGS_FILE), b"autosave=true\n").unwrap();
        fx.ready();
        let before = fx.snapshot();
        fx.check().verify().unwrap();
        fx.check().verify().unwrap();
        assert_eq!(fx.snapshot(), before, "the check must only read");
    }

    #[test]
    fn a_first_run_without_settings_still_passes_but_an_unusable_store_does_not() {
        let fx = fixture();
        fx.ready();
        fx.check().verify().unwrap();

        // The settings path exists but is a directory.
        fs::create_dir(fx.root.join(SETTINGS_FILE)).unwrap();
        assert_eq!(fx.check().verify().unwrap_err().code, ErrorCode::StateIncompatible);
        fs::remove_dir(fx.root.join(SETTINGS_FILE)).unwrap();

        // Larger than any settings file.
        fs::write(fx.root.join(SETTINGS_FILE), vec![b'x'; 2 * 1024 * 1024]).unwrap();
        assert_eq!(fx.check().verify().unwrap_err().code, ErrorCode::LimitExceeded);
    }

    #[test]
    fn a_missing_or_damaged_ticket_is_never_healthy() {
        let fx = fixture();
        publish_binding(&fx.coordination, &fx.transaction, &fx.root).unwrap();
        // No ticket at all: nothing issues one yet.
        assert!(fx.check().verify().is_err());

        for bad in [
            b"".to_vec(),
            b"not json".to_vec(),
            b"{}".to_vec(),
            vec![b' '; 70_000],
        ] {
            fx.write_private(TICKET_FILE, &bad);
            assert!(fx.check().verify().is_err());
        }

        // Unknown fields (a smuggled command or environment) are refused.
        let mut value = serde_json::to_value(fx.ticket()).unwrap();
        value["command"] = serde_json::json!("calc.exe");
        fx.write_private(TICKET_FILE, &serde_json::to_vec(&value).unwrap());
        assert!(fx.check().verify().is_err());

        // A duplicate key is refused too.
        let text = serde_json::to_string(&fx.ticket()).unwrap().replacen('{', "{\"schema\":1,", 1);
        fx.write_private(TICKET_FILE, text.as_bytes());
        assert!(fx.check().verify().is_err());

        // Restoring the valid ticket passes again.
        fx.write_ticket(&fx.ticket());
        fx.check().verify().unwrap();
    }

    #[test]
    fn a_ticket_for_another_schema_identity_or_root_is_refused() {
        let fx = fixture();
        fx.ready();
        let foreign_txn = TransactionId::generate().unwrap();
        let other_root = fx.dir.path().join("other-state");
        private_dir(&other_root);

        let mut schema = fx.ticket();
        schema.schema = 2;
        let mut install = fx.ticket();
        install.install_id = "a".repeat(32);
        let mut txn = fx.ticket();
        txn.transaction_id = foreign_txn.as_str().to_owned();
        let mut root = fx.ticket();
        root.state_root = other_root.to_str().unwrap().to_owned();
        let mut relative_root = fx.ticket();
        relative_root.state_root = "state".to_owned();

        for bad in [schema, install, txn, root, relative_root] {
            fx.write_ticket(&bad);
            assert!(fx.check().verify().is_err(), "{bad:?}");
        }
    }

    #[test]
    fn a_ticket_beyond_its_limits_or_with_bad_paths_is_refused() {
        let fx = fixture();
        fx.ready();
        let file = |path: &str| TicketFile {
            path: path.to_owned(),
            scroll_line: 0,
            caret_offset: 0,
        };
        let good = fx.ticket().files[0].path.clone();

        let mut too_many = fx.ticket();
        too_many.files = (0..=MAX_TICKET_FILES).map(|_| file(&good)).collect();
        let mut relative = fx.ticket();
        relative.files = vec![file("relative.md")];
        let mut parent = fx.ticket();
        parent.files = vec![file(&format!("{good}/../b.md"))];
        let mut far = fx.ticket();
        far.files[0].scroll_line = MAX_POSITION + 1;
        let mut dangling_active = fx.ticket();
        dangling_active.active = Some(1);
        let mut folder = fx.ticket();
        folder.work_folder = Some("work".to_owned());
        let mut long = fx.ticket();
        long.files = vec![file(&format!("{good}{}", "a".repeat(MAX_TICKET_PATH_BYTES)))];

        for bad in [too_many, relative, parent, far, dangling_active, folder, long] {
            fx.write_ticket(&bad);
            assert!(fx.check().verify().is_err());
        }
    }

    #[test]
    fn a_root_swapped_after_binding_is_refused() {
        let fx = fixture();
        fx.ready();
        let other = fx.dir.path().join("swapped");
        private_dir(&other);

        // The child holds a different root than the helper bound.
        let swapped = StateCheck::new(fx.coordination.clone(), fx.transaction.clone(), other.clone());
        assert!(swapped.verify().is_err());

        // The binding was replaced by another valid root after the helper read it.
        publish_binding(&fx.coordination, &fx.transaction, &other).unwrap();
        assert!(fx.check().verify().is_err());

        // The bound root vanished.
        publish_binding(&fx.coordination, &fx.transaction, &fx.root).unwrap();
        fx.check().verify().unwrap();
        fs::remove_dir_all(&fx.root).unwrap();
        assert!(fx.check().verify().is_err());
    }

    // ------------------------------------------------- issue and restore

    fn content(fx: &Fixture) -> TicketContent {
        let ticket = fx.ticket();
        TicketContent {
            work_folder: ticket.work_folder,
            files: ticket.files,
            active: ticket.active,
            explorer: ticket.explorer,
        }
    }

    fn restore(fx: &Fixture) -> TicketRestore {
        TicketRestore::new(fx.coordination.clone(), fx.transaction.clone(), fx.root.clone())
    }

    /// Writes a journal of this transaction that has reached `phase`.
    fn journal_at(fx: &Fixture, phase: Phase) {
        use crate::identity::{ApplyPlan, FileIdentity, PlanStep, ReleaseIdentity};
        use crate::journal::{DurableJournal, UpdateJournal};

        let release = |version: &str| ReleaseIdentity {
            target: "x86_64-pc-windows-msvc".to_owned(),
            version: version.to_owned(),
            catalog_sha256: None,
            inventory_sha256: None,
            bundle_id: None,
            team_id: None,
        };
        let plan = ApplyPlan {
            install_id: install_id(),
            transaction_id: fx.transaction.clone(),
            source: release("0.25.0"),
            destination: release("0.26.0"),
            steps: vec![PlanStep {
                rel_path: "a.dll".to_owned(),
                old: Some(FileIdentity::of(b"old")),
                new: Some(FileIdentity::of(b"new")),
                backup_rel: Some("backup/a.dll".to_owned()),
            }],
            unchanged: Vec::new(),
        };
        let mut journal = DurableJournal::create(
            FileJournalStore::new(fx.coordination.journal_path(&fx.transaction)),
            UpdateJournal::begin(&plan, None).unwrap(),
        )
        .unwrap();
        for step in [
            Phase::ExitAuthorized,
            Phase::Applying,
            Phase::AwaitingHealth,
            Phase::Committed,
        ] {
            journal.advance_phase(step).unwrap();
            if step == phase {
                break;
            }
        }
    }

    #[test]
    fn an_issued_ticket_is_what_the_child_verifies_and_contains_no_text() {
        let fx = fixture();
        publish_binding(&fx.coordination, &fx.transaction, &fx.root).unwrap();
        issue_ticket(&fx.coordination, &fx.transaction, &fx.root, content(&fx), || Ok(())).unwrap();
        fx.check().verify().unwrap();

        let raw = fs::read_to_string(
            fx.coordination.transaction_dir(&fx.transaction).join(TICKET_FILE),
        )
        .unwrap();
        let value: serde_json::Value = serde_json::from_str(&raw).unwrap();
        let mut keys: Vec<_> = value.as_object().unwrap().keys().cloned().collect();
        keys.sort();
        assert_eq!(
            keys,
            [
                "active",
                "explorer",
                "files",
                "install_id",
                "schema",
                "state_root",
                "transaction_id",
                "work_folder"
            ]
        );
        // Without an Explorer registration the key is present and null; the
        // ticket still holds only paths and identifiers, never document text.
        assert!(value["explorer"].is_null());
    }

    #[test]
    fn a_stale_permit_or_failed_recheck_leaves_no_ticket() {
        let fx = fixture();
        publish_binding(&fx.coordination, &fx.transaction, &fx.root).unwrap();
        let path = fx.coordination.transaction_dir(&fx.transaction).join(TICKET_FILE);

        let refused = issue_ticket(&fx.coordination, &fx.transaction, &fx.root, content(&fx), || {
            Err(UpdateError::new(ErrorCode::StaleJournal, "the permit is stale"))
        });
        assert_eq!(refused.unwrap_err().code, ErrorCode::StaleJournal);
        assert!(!path.exists());
        assert!(fx.check().verify().is_err());

        // A previously issued ticket does not survive a refused re-issue as a valid one.
        issue_ticket(&fx.coordination, &fx.transaction, &fx.root, content(&fx), || Ok(())).unwrap();
        assert!(path.exists());
    }

    #[test]
    fn an_invalid_ticket_is_never_asked_about_its_permit_or_written() {
        let fx = fixture();
        publish_binding(&fx.coordination, &fx.transaction, &fx.root).unwrap();
        let path = fx.coordination.transaction_dir(&fx.transaction).join(TICKET_FILE);
        let asked = std::cell::Cell::new(false);

        let mut relative = content(&fx);
        relative.files[0].path = "relative.md".to_owned();
        let mut dangling = content(&fx);
        dangling.active = Some(9);
        let mut many = content(&fx);
        many.files = (0..=MAX_TICKET_FILES).map(|_| many.files[0].clone()).collect();
        for bad in [relative, dangling, many] {
            let outcome = issue_ticket(&fx.coordination, &fx.transaction, &fx.root, bad, || {
                asked.set(true);
                Ok(())
            });
            assert!(outcome.is_err());
        }
        assert!(!asked.get());
        assert!(!path.exists());
    }

    #[test]
    fn a_ticket_for_an_unbound_or_other_root_is_not_issued() {
        let fx = fixture();
        // No binding yet.
        assert!(
            issue_ticket(&fx.coordination, &fx.transaction, &fx.root, content(&fx), || Ok(())).is_err()
        );
        publish_binding(&fx.coordination, &fx.transaction, &fx.root).unwrap();
        let other = fx.dir.path().join("other-state");
        private_dir(&other);
        assert!(
            issue_ticket(&fx.coordination, &fx.transaction, &other, content(&fx), || Ok(())).is_err()
        );
        assert!(
            !fx.coordination
                .transaction_dir(&fx.transaction)
                .join(TICKET_FILE)
                .exists()
        );
    }

    #[test]
    fn the_ticket_is_used_only_after_a_durable_commit_and_only_once() {
        let fx = fixture();
        fx.ready();

        // Nothing is taken before the journal says Committed.
        assert!(restore(&fx).take().is_err());
        journal_at(&fx, Phase::AwaitingHealth);
        assert!(restore(&fx).take().is_err());
        assert!(
            fx.coordination
                .transaction_dir(&fx.transaction)
                .join(TICKET_FILE)
                .exists(),
            "an uncommitted update must not consume the ticket"
        );

        // Replace the journal by a committed one.
        let _ = fs::remove_file(fx.coordination.journal_path(&fx.transaction));
        journal_at(&fx, Phase::Committed);
        let taken = restore(&fx).take().unwrap().expect("a ticket");
        assert_eq!(taken, fx.ticket());
        // A second start (or a restart after a crash) restores nothing again.
        assert!(restore(&fx).take().unwrap().is_none());
        assert!(restore(&fx).take().unwrap().is_none());
    }

    fn explorer(file_enabled: bool, folder_enabled: bool) -> ExplorerTicket {
        ExplorerTicket {
            owner: "92b8fc05-dc63-44ed-9afc-fb10a36bee8a".to_owned(),
            package_name: "Hane.ShellIntegration".to_owned(),
            publisher: "CN=Hane".to_owned(),
            file_enabled,
            folder_enabled,
        }
    }

    #[test]
    fn the_explorer_registration_survives_the_ticket_for_each_enabled_combination() {
        for (file, folder) in [(true, true), (true, false), (false, true)] {
            let fx = fixture();
            publish_binding(&fx.coordination, &fx.transaction, &fx.root).unwrap();
            let mut content = content(&fx);
            content.explorer = Some(explorer(file, folder));
            issue_ticket(&fx.coordination, &fx.transaction, &fx.root, content, || Ok(())).unwrap();
            journal_at(&fx, Phase::Committed);
            let taken = restore(&fx).take().unwrap().expect("a ticket");
            assert_eq!(taken.explorer, Some(explorer(file, folder)));
        }
    }

    #[test]
    fn a_user_without_the_integration_gets_a_ticket_without_it() {
        let fx = fixture();
        publish_binding(&fx.coordination, &fx.transaction, &fx.root).unwrap();
        issue_ticket(&fx.coordination, &fx.transaction, &fx.root, content(&fx), || Ok(())).unwrap();
        journal_at(&fx, Phase::Committed);
        assert_eq!(restore(&fx).take().unwrap().expect("a ticket").explorer, None);
    }

    #[test]
    fn an_explorer_registration_with_nothing_enabled_or_a_bad_field_is_not_issued() {
        let fx = fixture();
        publish_binding(&fx.coordination, &fx.transaction, &fx.root).unwrap();
        for bad in [
            explorer(false, false),
            ExplorerTicket {
                owner: String::new(),
                ..explorer(true, true)
            },
            ExplorerTicket {
                publisher: "x".repeat(MAX_EXPLORER_FIELD_BYTES + 1),
                ..explorer(true, true)
            },
        ] {
            let mut content = content(&fx);
            content.explorer = Some(bad);
            assert!(
                issue_ticket(&fx.coordination, &fx.transaction, &fx.root, content, || Ok(())).is_err()
            );
        }
        assert!(!restore(&fx).has_unclaimed_ticket());
    }

    #[test]
    fn a_claim_that_is_not_durable_is_never_handed_out() {
        let fx = fixture();
        fx.ready();
        journal_at(&fx, Phase::Committed);

        let mut barrier_ran = false;
        let failed = restore(&fx).take_with(|claimed| {
            barrier_ran = true;
            // The rename is visible, but the barrier has not confirmed it.
            assert!(claimed.exists());
            Err(std::io::Error::other("sync failed"))
        });
        assert!(barrier_ran);
        assert!(failed.is_err(), "take must not succeed before the claim is durable");
        // The ticket is back and unclaimed, so a restart can retry once.
        assert!(restore(&fx).has_unclaimed_ticket());

        let taken = restore(&fx).take().unwrap().expect("retry after the failed barrier");
        assert_eq!(taken, fx.ticket());
        // Handed out exactly once.
        assert!(restore(&fx).take().unwrap().is_none());
    }

    #[test]
    fn a_ticket_is_returned_only_after_the_barrier_confirmed_the_claim() {
        let fx = fixture();
        fx.ready();
        journal_at(&fx, Phase::Committed);
        let mut calls = 0;
        let taken = restore(&fx)
            .take_with(|claimed| {
                calls += 1;
                assert!(claimed.exists());
                Ok(())
            })
            .unwrap();
        assert_eq!(calls, 1);
        assert_eq!(taken, Some(fx.ticket()));
        assert!(restore(&fx).take().unwrap().is_none());
    }

    #[test]
    fn a_damaged_ticket_is_consumed_and_never_retried() {
        let fx = fixture();
        publish_binding(&fx.coordination, &fx.transaction, &fx.root).unwrap();
        fx.write_private(TICKET_FILE, b"not json");
        journal_at(&fx, Phase::Committed);
        assert!(restore(&fx).take().is_err());
        assert!(restore(&fx).take().unwrap().is_none());
    }

    #[test]
    fn a_missing_ticket_restores_nothing() {
        let fx = fixture();
        publish_binding(&fx.coordination, &fx.transaction, &fx.root).unwrap();
        journal_at(&fx, Phase::Committed);
        assert!(restore(&fx).take().unwrap().is_none());
    }

    #[test]
    fn a_cleanup_failure_is_pending_and_can_be_retried() {
        let fx = fixture();
        fx.ready();
        journal_at(&fx, Phase::Committed);
        assert!(restore(&fx).take().unwrap().is_some());

        let failed = restore(&fx).finish_with(|_| {
            Err(std::io::Error::new(std::io::ErrorKind::PermissionDenied, "busy"))
        });
        assert!(matches!(failed, Cleanup::CleanupPending(_)));
        // The ticket stays used while the cleanup is pending.
        assert!(restore(&fx).take().unwrap().is_none());

        assert_eq!(restore(&fx).finish(), Cleanup::Done);
        assert_eq!(restore(&fx).finish(), Cleanup::Done);
        assert!(restore(&fx).take().unwrap().is_none());
    }

    #[test]
    fn a_rebound_root_is_refused_at_restore() {
        let fx = fixture();
        fx.ready();
        journal_at(&fx, Phase::Committed);
        let other = fx.dir.path().join("swapped");
        private_dir(&other);
        let swapped = TicketRestore::new(fx.coordination.clone(), fx.transaction.clone(), other);
        assert!(swapped.take().is_err());
    }
}
