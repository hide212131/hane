//! The trusted helper identity of a protected-placement update (Issue #510).
//!
//! Recovery of an interrupted update on a protected placement elevates the
//! helper copy that was staged before the first mutation, pinned to the identity
//! of the *old* signed helper. That identity cannot be re-derived at recovery
//! time: once the apply replaced the helper or the signed inventory, the live
//! install no longer yields it. The elevated helper therefore records it before
//! anything is staged or mutated, in the coordination parent, which only an
//! elevated process can write to for a protected placement. The file is bound to
//! the `InstallId` and the `TransactionId` by its name and its content, owned by
//! a system-protected principal, and verified as such on every read; a record
//! that is missing, altered, foreign or not verifiably protected is an error and
//! never a reason to elevate. Nothing under the user-writable staging directory
//! is a trust anchor.
//!
//! The record is retired once its transaction is terminal (or its directory is
//! gone); a stale record is inert because recovery asks for one transaction.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use crate::error::{ErrorCode, Result, UpdateError};
use crate::fsutil::{read_limited, remove_file_durable, rename_new_durable};
use crate::identity::{FileIdentity, InstallId, TransactionId};
use crate::journal::{FileJournalStore, JournalStore, Phase};
use crate::platform::lease::Coordination;

const MAGIC: &str = "hane-update-helper-anchor-v1";
const RECORD_LIMIT: u64 = 1024;

/// Checks that a path is a protected entry; injected so tests need no elevation.
pub(crate) type VerifyProtected<'a> = &'a dyn Fn(&Path) -> Result<()>;

/// The production protection check. Tests cannot create files owned by a
/// system principal, so a test build checks only that the entry is a plain file,
/// unless the test asks for a refusal with [`test_support::refuse_protection`].
#[cfg(not(test))]
fn protected(path: &Path) -> Result<()> {
    crate::platform::acl::verify_protected(path)
}

#[cfg(test)]
fn protected(path: &Path) -> Result<()> {
    if test_support::REFUSE.with(std::cell::Cell::get) {
        return Err(UpdateError::new(ErrorCode::UnsupportedInstall, "not protected"));
    }
    let meta = fs::symlink_metadata(path)?;
    if meta.file_type().is_symlink() || !meta.is_file() {
        return Err(UpdateError::new(ErrorCode::UnsupportedInstall, "not a plain file"));
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod test_support {
    use std::cell::Cell;

    thread_local! {
        pub(super) static REFUSE: Cell<bool> = const { Cell::new(false) };
    }

    /// Makes the protection check of this thread refuse (or accept again).
    pub(crate) fn refuse_protection(refuse: bool) {
        REFUSE.with(|cell| cell.set(refuse));
    }
}

fn malformed() -> UpdateError {
    UpdateError::new(ErrorCode::IdentityMismatch, "the helper identity record is malformed")
}

fn file_name(install_id: &InstallId, transaction: &TransactionId) -> String {
    format!("{}.anchor-{}", install_id.coordination_dir_name(), transaction.as_str())
}

pub(crate) fn anchor_path(parent: &Path, install_id: &InstallId, transaction: &TransactionId) -> PathBuf {
    parent.join(file_name(install_id, transaction))
}

fn parent_of(coordination: &Coordination) -> Result<&Path> {
    coordination.dir().parent().ok_or_else(|| {
        UpdateError::new(ErrorCode::Io, "the coordination directory has no parent")
    })
}

fn is_sha256_hex(text: &str) -> bool {
    text.len() == 64 && text.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Records `identity` for `transaction` beside the coordination directory. The
/// elevated helper calls it before the first staged file and before any install
/// mutation; an error means nothing was recorded and the update must not go on.
pub(crate) fn write(
    coordination: &Coordination,
    transaction: &TransactionId,
    identity: &FileIdentity,
) -> Result<()> {
    write_with(parent_of(coordination)?, coordination.install_id(), transaction, identity, &protected)
}

/// [`write`] with the protection check injected. The record is written to a
/// temp file, flushed, proven protected, and only then moved onto its name
/// durably without replacing; it is proven again at its final name.
pub(crate) fn write_with(
    parent: &Path,
    install_id: &InstallId,
    transaction: &TransactionId,
    identity: &FileIdentity,
    verify: VerifyProtected<'_>,
) -> Result<()> {
    let path = anchor_path(parent, install_id, transaction);
    match fs::symlink_metadata(&path) {
        Ok(_) => {
            return Err(UpdateError::new(
                ErrorCode::IdentityMismatch,
                "a helper identity record already exists for this transaction",
            ));
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Err(err) => return Err(err.into()),
    }
    let temp = parent.join(format!(".{}.tmp", file_name(install_id, transaction)));
    match fs::remove_file(&temp) {
        Ok(()) => {}
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Err(err) => return Err(err.into()),
    }
    let text = format!(
        "{MAGIC}\n{}\n{}\n{}\n{}\n",
        install_id.as_str(),
        transaction.as_str(),
        identity.size,
        identity.sha256
    );
    let prepared = (|| -> Result<()> {
        let mut file = fs::OpenOptions::new().write(true).create_new(true).open(&temp)?;
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        drop(file);
        verify(&temp)
    })();
    if let Err(err) = prepared {
        let _ = fs::remove_file(&temp);
        return Err(err);
    }
    if let Err(err) = rename_new_durable(&temp, &path) {
        let _ = fs::remove_file(&temp);
        return Err(UpdateError::new(
            ErrorCode::Io,
            format!("the helper identity record could not be published durably: {err}"),
        ));
    }
    if let Err(err) = verify(&path) {
        let _ = remove_file_durable(&path);
        return Err(err);
    }
    Ok(())
}

/// The identity recorded for `transaction`. A missing, unprotected, malformed or
/// foreign record is an error.
pub(crate) fn read(coordination: &Coordination, transaction: &TransactionId) -> Result<FileIdentity> {
    read_with(parent_of(coordination)?, coordination.install_id(), transaction, &protected)
}

/// [`read`] with the protection check injected.
pub(crate) fn read_with(
    parent: &Path,
    install_id: &InstallId,
    transaction: &TransactionId,
    verify: VerifyProtected<'_>,
) -> Result<FileIdentity> {
    let path = anchor_path(parent, install_id, transaction);
    let Some(raw) = read_limited(&path, RECORD_LIMIT)? else {
        return Err(UpdateError::new(
            ErrorCode::NotFound,
            "the helper identity record of this transaction is missing",
        ));
    };
    verify(&path)?;
    let text = String::from_utf8(raw).map_err(|_| malformed())?;
    let mut lines = text.lines();
    let (Some(magic), Some(install), Some(txn), Some(size), Some(sha256), None) = (
        lines.next(),
        lines.next(),
        lines.next(),
        lines.next(),
        lines.next(),
        lines.next(),
    ) else {
        return Err(malformed());
    };
    if magic != MAGIC || !is_sha256_hex(sha256) {
        return Err(malformed());
    }
    if install != install_id.as_str() || txn != transaction.as_str() {
        return Err(UpdateError::new(
            ErrorCode::IdentityMismatch,
            "the helper identity record belongs to another install or transaction",
        ));
    }
    Ok(FileIdentity {
        size: size.parse().map_err(|_| malformed())?,
        sha256: sha256.to_owned(),
    })
}

/// Removes the record of `transaction`; a missing record is not an error. An
/// unprivileged caller in a protected directory fails, which leaves an inert
/// record for a later elevated sweep.
pub(crate) fn retire(coordination: &Coordination, transaction: &TransactionId) -> Result<()> {
    retire_in(parent_of(coordination)?, coordination.install_id(), transaction)
}

pub(crate) fn retire_in(
    parent: &Path,
    install_id: &InstallId,
    transaction: &TransactionId,
) -> Result<()> {
    match remove_file_durable(&anchor_path(parent, install_id, transaction)) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(UpdateError::new(
            ErrorCode::Io,
            format!("the helper identity record could not be retired: {err}"),
        )),
    }
}

/// Retires the records of this install whose transaction is settled: its journal
/// is terminal, or its transaction directory no longer exists. Records of a
/// pending or unjournaled live transaction stay. Best effort; whatever cannot
/// be read or removed stays for the next sweep.
pub(crate) fn sweep_settled(coordination: &Coordination) {
    let Ok(parent) = parent_of(coordination) else {
        return;
    };
    let Ok(entries) = fs::read_dir(parent) else {
        return;
    };
    let prefix = format!("{}.anchor-", coordination.install_id().coordination_dir_name());
    for entry in entries.flatten() {
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let Some(id) = name.strip_prefix(&prefix) else {
            continue;
        };
        let Ok(transaction) = TransactionId::parse(id) else {
            continue;
        };
        let settled = match FileJournalStore::new(coordination.journal_path(&transaction)).load() {
            Ok(Some(journal)) => {
                journal.transaction_id == transaction
                    && &journal.install_id == coordination.install_id()
                    && matches!(journal.phase, Phase::Committed | Phase::RolledBack)
            }
            Ok(None) => matches!(
                fs::symlink_metadata(coordination.transaction_dir(&transaction)),
                Err(ref err) if err.kind() == io::ErrorKind::NotFound
            ),
            Err(_) => false,
        };
        if settled {
            let _ = retire(coordination, &transaction);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::InstallFacts;
    use crate::testing::TempDir;

    fn install_id(path: &str) -> InstallId {
        InstallId::derive(&InstallFacts {
            os: "test",
            volume_id: "v",
            install_path: path,
            owner: "u",
        })
        .unwrap()
    }

    fn accept(path: &Path) -> Result<()> {
        assert!(fs::symlink_metadata(path).unwrap().is_file());
        Ok(())
    }

    fn reject(_: &Path) -> Result<()> {
        Err(UpdateError::new(ErrorCode::UnsupportedInstall, "not protected"))
    }

    #[test]
    fn a_record_round_trips_and_is_bound_to_install_and_transaction() {
        let dir = TempDir::new("anchor-roundtrip");
        let (install, other_install) = (install_id("/x"), install_id("/y"));
        let transaction = TransactionId::generate().unwrap();
        let identity = FileIdentity::of(b"old helper");
        write_with(dir.path(), &install, &transaction, &identity, &accept).unwrap();
        assert_eq!(read_with(dir.path(), &install, &transaction, &accept).unwrap(), identity);

        // Another transaction or install has no record, and a renamed record
        // is refused by its own content.
        let other = TransactionId::generate().unwrap();
        assert_eq!(
            read_with(dir.path(), &install, &other, &accept).unwrap_err().code,
            ErrorCode::NotFound
        );
        fs::copy(
            anchor_path(dir.path(), &install, &transaction),
            anchor_path(dir.path(), &install, &other),
        )
        .unwrap();
        assert_eq!(
            read_with(dir.path(), &install, &other, &accept).unwrap_err().code,
            ErrorCode::IdentityMismatch
        );
        fs::copy(
            anchor_path(dir.path(), &install, &transaction),
            anchor_path(dir.path(), &other_install, &transaction),
        )
        .unwrap();
        assert_eq!(
            read_with(dir.path(), &other_install, &transaction, &accept).unwrap_err().code,
            ErrorCode::IdentityMismatch
        );
    }

    #[test]
    fn an_altered_or_unprotected_record_is_an_error() {
        let dir = TempDir::new("anchor-tamper");
        let install = install_id("/x");
        let transaction = TransactionId::generate().unwrap();
        let identity = FileIdentity::of(b"old helper");
        write_with(dir.path(), &install, &transaction, &identity, &accept).unwrap();
        let path = anchor_path(dir.path(), &install, &transaction);

        assert!(read_with(dir.path(), &install, &transaction, &reject).is_err());
        for damaged in [
            "garbage\n".to_owned(),
            format!("{MAGIC}\n{}\n{}\n10\nnot-a-hash\n", install.as_str(), transaction.as_str()),
            format!("{MAGIC}\n{}\n{}\nx\n{}\n", install.as_str(), transaction.as_str(), identity.sha256),
            format!("{MAGIC}\n{}\n{}\n1\n{}\nextra\n", install.as_str(), transaction.as_str(), identity.sha256),
        ] {
            fs::write(&path, damaged).unwrap();
            assert!(read_with(dir.path(), &install, &transaction, &accept).is_err());
        }
    }

    #[test]
    fn a_failed_protection_check_publishes_nothing_and_an_existing_record_is_not_replaced() {
        let dir = TempDir::new("anchor-write-fail");
        let install = install_id("/x");
        let transaction = TransactionId::generate().unwrap();
        let identity = FileIdentity::of(b"old helper");
        assert!(write_with(dir.path(), &install, &transaction, &identity, &reject).is_err());
        assert!(!anchor_path(dir.path(), &install, &transaction).exists());
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0, "no temp file stays");

        write_with(dir.path(), &install, &transaction, &identity, &accept).unwrap();
        let newer = FileIdentity::of(b"new helper");
        assert!(write_with(dir.path(), &install, &transaction, &newer, &accept).is_err());
        assert_eq!(read_with(dir.path(), &install, &transaction, &accept).unwrap(), identity);
    }

    #[test]
    fn retiring_removes_the_record_and_a_missing_one_is_fine() {
        let dir = TempDir::new("anchor-retire");
        let install = install_id("/x");
        let transaction = TransactionId::generate().unwrap();
        write_with(dir.path(), &install, &transaction, &FileIdentity::of(b"a"), &accept).unwrap();
        retire_in(dir.path(), &install, &transaction).unwrap();
        assert!(!anchor_path(dir.path(), &install, &transaction).exists());
        retire_in(dir.path(), &install, &transaction).unwrap();
    }
}
