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
use crate::fsutil::{durable_write, read_limited};
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

/// The only variables that may reach the limited child. They are what a normal
/// Hane start needs to find the same user profile (settings, AI configuration,
/// the named pipe derived from `LOCALAPPDATA`). Loader and search-path variables
/// (`LD_PRELOAD`, `DYLD_*`, `PATH`, ...) are deliberately absent.
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
];

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ChildEnvironment {
    schema: u32,
    install_id: String,
    transaction_id: String,
    vars: std::collections::BTreeMap<String, String>,
}

fn environment_path(coordination: &Coordination, transaction: &TransactionId) -> PathBuf {
    coordination.transaction_dir(transaction).join(ENVIRONMENT_FILE)
}

fn env_value_ok(value: &str) -> bool {
    !value.is_empty() && value.len() <= MAX_ENV_VALUE_BYTES && !value.chars().any(char::is_control)
}

/// Parent side: records the allowlisted variables of the real parent in the
/// owner-only transaction directory. Anything absent, not text, empty, too long
/// or with control characters is simply left out.
pub fn publish_environment(
    coordination: &Coordination,
    transaction: &TransactionId,
    lookup: impl Fn(&str) -> Option<std::ffi::OsString>,
) -> Result<()> {
    let vars = CHILD_ENV_ALLOWLIST
        .iter()
        .filter_map(|name| {
            let value = lookup(name)?.into_string().ok()?;
            env_value_ok(&value).then(|| ((*name).to_owned(), value))
        })
        .collect();
    let document = ChildEnvironment {
        schema: BINDING_SCHEMA,
        install_id: coordination.install_id().as_str().to_owned(),
        transaction_id: transaction.as_str().to_owned(),
        vars,
    };
    let bytes = serde_json::to_vec(&document)
        .map_err(|err| UpdateError::new(ErrorCode::Io, err.to_string()))?;
    discard_environment(coordination, transaction);
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
pub fn read_environment(
    coordination: &Coordination,
    transaction: &TransactionId,
) -> Result<Vec<(String, String)>> {
    let path = environment_path(coordination, transaction);
    if fs::symlink_metadata(&path).is_err() {
        return Ok(Vec::new());
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
        if !CHILD_ENV_ALLOWLIST.contains(&name.as_str()) || !env_value_ok(&value) {
            return Err(mismatch("the child environment names a variable that is not allowed"));
        }
        vars.push((name, value));
    }
    Ok(vars)
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TicketContent {
    pub work_folder: Option<String>,
    pub files: Vec<TicketFile>,
    /// Index into `files` of the file that was active.
    pub active: Option<u32>,
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
        match fs::rename(ticket_path(&self.coordination, &self.transaction), &claimed) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(err) => return Err(err.into()),
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

        let ambient = |name: &str| -> Option<std::ffi::OsString> {
            match name {
                "HOME" => Some("/home/u".into()),
                "LOCALAPPDATA" => Some("C:\\Users\\u\\AppData\\Local".into()),
                "TMP" => Some("bad\nvalue".into()),
                "TEMP" => Some("x".repeat(MAX_ENV_VALUE_BYTES + 1).into()),
                _ => None,
            }
        };
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
        for name in ["LD_PRELOAD", "DYLD_INSERT_LIBRARIES", "PATH", "HANE_COMMAND"] {
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
            ["active", "files", "install_id", "schema", "state_root", "transaction_id", "work_folder"]
        );
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
