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
use crate::platform::acl;
use crate::platform::lease::Coordination;
use crate::util::parse_strict;

pub const BINDING_FILE: &str = "state-root.binding";
pub const TICKET_FILE: &str = "restart-ticket.json";
pub const BINDING_SCHEMA: u32 = 1;
pub const TICKET_SCHEMA: u32 = 1;

/// Longest state root accepted anywhere on the way to the child.
pub const MAX_ROOT_BYTES: usize = 1024;
const BINDING_LIMIT: u64 = 4096;
const TICKET_LIMIT: u64 = 64 * 1024;
const SETTINGS_FILE: &str = "settings.conf";
const SETTINGS_LIMIT: u64 = 1024 * 1024;
const MAX_TICKET_FILES: usize = 64;
const MAX_TICKET_PATH_BYTES: usize = 2048;
const MAX_POSITION: u32 = 100_000_000;

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

fn ticket_path_ok(text: &str) -> bool {
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
}
