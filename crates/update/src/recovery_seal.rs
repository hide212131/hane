//! The trusted recovery plan of a protected-placement update (Issue #510).
//!
//! The journal and the backups of a transaction live in the staging directory,
//! which the requesting account's ordinary process can write. A rollback run by
//! the elevated helper restores whatever the journal names from the backup that
//! matches the journal's own hashes, so a journal and a backup altered together
//! would let the same account write arbitrary bytes into the protected install.
//! Neither the journal's structure nor hashes that only agree with each other are
//! a trust root.
//!
//! The elevated helper therefore records, before the journal exists and before
//! the first install mutation, a digest of everything recovery acts on (the
//! relative paths, the old and new file identities, the backup paths and the
//! unchanged files; see [`UpdateJournal::recovery_digest`]) in the protected
//! coordination parent, bound to the `InstallId` and the `TransactionId`. It is
//! the same kind of record as the helper identity of [`crate::helper_anchor`] and
//! is verified as protected on every read. Before an elevated recovery changes
//! anything the loaded journal must reproduce the sealed digest; the old
//! identities it then names are the sealed ones, and the restore itself still
//! checks the backup content against them. A missing, altered, foreign or
//! unprotected seal, or a journal that does not match it, stops as
//! `RecoveryRequired` with the journal and the backups kept.
//!
//! The seal is required exactly for the transactions that have a helper identity
//! record, that is, the ones the elevated helper bootstrapped. It is retired
//! together with that record.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use crate::error::{ErrorCode, Result, UpdateError};
use crate::fsutil::{read_limited, remove_file_durable, rename_new_durable};
use crate::helper_anchor::{VerifyProtected, anchor_path};
use crate::identity::{InstallId, TransactionId};
use crate::journal::UpdateJournal;
use crate::platform::lease::Coordination;
use crate::util::is_sha256_hex;

const MAGIC: &str = "hane-update-recovery-seal-v1";
const RECORD_LIMIT: u64 = 1024;

fn malformed() -> UpdateError {
    UpdateError::new(ErrorCode::IdentityMismatch, "the recovery seal is malformed")
}

fn file_name(install_id: &InstallId, transaction: &TransactionId) -> String {
    format!("{}.seal-{}", install_id.coordination_dir_name(), transaction.as_str())
}

pub(crate) fn seal_path(parent: &Path, install_id: &InstallId, transaction: &TransactionId) -> PathBuf {
    parent.join(file_name(install_id, transaction))
}

fn parent_of(coordination: &Coordination) -> Result<&Path> {
    coordination.dir().parent().ok_or_else(|| {
        UpdateError::new(ErrorCode::Io, "the coordination directory has no parent")
    })
}

/// Whether this transaction was bootstrapped by the elevated helper, which is
/// when its recovery plan must be sealed.
fn required_in(parent: &Path, install_id: &InstallId, transaction: &TransactionId) -> Result<bool> {
    match fs::symlink_metadata(anchor_path(parent, install_id, transaction)) {
        Ok(_) => Ok(true),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(err) => Err(err.into()),
    }
}

/// Seals `journal` if this transaction is a protected one; an error means
/// nothing was sealed and the update must not go on. Called before the journal
/// is created.
pub(crate) fn seal_if_protected(coordination: &Coordination, journal: &UpdateJournal) -> Result<()> {
    let parent = parent_of(coordination)?;
    if !required_in(parent, coordination.install_id(), &journal.transaction_id)? {
        return Ok(());
    }
    write_with(parent, journal, &crate::helper_anchor::protected)
}

/// Checks the loaded journal against the seal of a protected transaction.
/// Transactions that are not protected need no seal and pass.
pub(crate) fn verify_if_protected(coordination: &Coordination, journal: &UpdateJournal) -> Result<()> {
    let parent = parent_of(coordination)?;
    if !required_in(parent, coordination.install_id(), &journal.transaction_id)? {
        return Ok(());
    }
    verify_with(parent, journal, &crate::helper_anchor::protected)
}

/// [`seal_if_protected`] for a protected transaction, with the protection check
/// injected. The record is written to a temp file, flushed, proven protected, and
/// only then moved onto its name durably without replacing.
pub(crate) fn write_with(parent: &Path, journal: &UpdateJournal, verify: VerifyProtected<'_>) -> Result<()> {
    let (install_id, transaction) = (&journal.install_id, &journal.transaction_id);
    let path = seal_path(parent, install_id, transaction);
    match fs::symlink_metadata(&path) {
        Ok(_) => {
            return Err(UpdateError::new(
                ErrorCode::IdentityMismatch,
                "a recovery seal already exists for this transaction",
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
        "{MAGIC}\n{}\n{}\n{}\n",
        install_id.as_str(),
        transaction.as_str(),
        journal.recovery_digest()?
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
            format!("the recovery seal could not be published durably: {err}"),
        ));
    }
    if let Err(err) = verify(&path) {
        let _ = remove_file_durable(&path);
        return Err(err);
    }
    Ok(())
}

/// The journal must reproduce the digest sealed for exactly its install and
/// transaction. Any missing, unprotected, malformed, foreign or differing seal
/// is an error.
pub(crate) fn verify_with(parent: &Path, journal: &UpdateJournal, verify: VerifyProtected<'_>) -> Result<()> {
    let (install_id, transaction) = (&journal.install_id, &journal.transaction_id);
    let path = seal_path(parent, install_id, transaction);
    let Some(raw) = read_limited(&path, RECORD_LIMIT)? else {
        return Err(UpdateError::new(
            ErrorCode::NotFound,
            "the recovery seal of this transaction is missing",
        ));
    };
    verify(&path)?;
    let text = String::from_utf8(raw).map_err(|_| malformed())?;
    let mut lines = text.lines();
    let (Some(magic), Some(install), Some(txn), Some(digest), None) =
        (lines.next(), lines.next(), lines.next(), lines.next(), lines.next())
    else {
        return Err(malformed());
    };
    if magic != MAGIC || !is_sha256_hex(digest) {
        return Err(malformed());
    }
    if install != install_id.as_str() || txn != transaction.as_str() {
        return Err(UpdateError::new(
            ErrorCode::IdentityMismatch,
            "the recovery seal belongs to another install or transaction",
        ));
    }
    if journal.recovery_digest()? != digest {
        return Err(UpdateError::new(
            ErrorCode::JournalInvalid,
            "the journal differs from the sealed recovery plan",
        ));
    }
    Ok(())
}

/// Removes the seal of `transaction`; a missing seal is not an error.
pub(crate) fn retire_in(parent: &Path, install_id: &InstallId, transaction: &TransactionId) -> Result<()> {
    match remove_file_durable(&seal_path(parent, install_id, transaction)) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(UpdateError::new(
            ErrorCode::Io,
            format!("the recovery seal could not be retired: {err}"),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::{FileIdentity, InstallFacts, ReleaseIdentity};
    use crate::journal::{JournalStep, Phase, StepState};
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

    fn release(version: &str) -> ReleaseIdentity {
        ReleaseIdentity {
            target: "x86_64-pc-windows-msvc".to_owned(),
            version: version.to_owned(),
            catalog_sha256: None,
            inventory_sha256: Some("0".repeat(64)),
            bundle_id: None,
            team_id: None,
        }
    }

    fn journal(install: &InstallId, transaction: &TransactionId) -> UpdateJournal {
        UpdateJournal {
            schema: crate::journal::JOURNAL_SCHEMA,
            transaction_id: transaction.clone(),
            install_id: install.clone(),
            plan_hash: "1".repeat(64),
            source: release("0.25.0"),
            destination: release("0.26.0"),
            ticket_sha256: None,
            phase: Phase::Applying,
            steps: vec![JournalStep {
                rel_path: "hane.exe".to_owned(),
                old: Some(FileIdentity::of(b"old exe")),
                new: Some(FileIdentity::of(b"new exe")),
                backup_rel: Some("backup/hane.exe".to_owned()),
                state: StepState::Intent,
            }],
            unchanged: Vec::new(),
            generation: 3,
        }
    }

    fn accept(path: &Path) -> Result<()> {
        assert!(fs::symlink_metadata(path).unwrap().is_file());
        Ok(())
    }

    fn reject(_: &Path) -> Result<()> {
        Err(UpdateError::new(ErrorCode::UnsupportedInstall, "not protected"))
    }

    #[test]
    fn a_journal_matches_its_seal_whatever_its_progress() {
        let dir = TempDir::new("seal-roundtrip");
        let install = install_id("/x");
        let transaction = TransactionId::generate().unwrap();
        let mut sealed = journal(&install, &transaction);
        sealed.phase = Phase::Prepared;
        sealed.steps[0].state = StepState::Planned;
        sealed.generation = 0;
        write_with(dir.path(), &sealed, &accept).unwrap();

        let mut progressed = journal(&install, &transaction);
        progressed.phase = Phase::RollingBack;
        progressed.steps[0].state = StepState::Applied;
        progressed.generation = 9;
        verify_with(dir.path(), &progressed, &accept).unwrap();
    }

    #[test]
    fn a_forged_plan_is_refused_whatever_part_of_it_is_changed() {
        let dir = TempDir::new("seal-forged");
        let install = install_id("/x");
        let transaction = TransactionId::generate().unwrap();
        let genuine = journal(&install, &transaction);
        write_with(dir.path(), &genuine, &accept).unwrap();

        // The forged old identity matches a forged backup, so the journal is
        // internally consistent; only the seal tells the two apart.
        let mut forged_old = genuine.clone();
        forged_old.steps[0].old = Some(FileIdentity::of(b"attacker bytes"));
        let mut forged_path = genuine.clone();
        forged_path.steps[0].rel_path = "evil.dll".to_owned();
        forged_path.steps[0].backup_rel = Some("backup/evil.dll".to_owned());
        let mut forged_extra = genuine.clone();
        forged_extra.steps.push(JournalStep {
            rel_path: "extra.dll".to_owned(),
            old: Some(FileIdentity::of(b"attacker bytes")),
            new: None,
            backup_rel: Some("backup/extra.dll".to_owned()),
            state: StepState::Applied,
        });
        let mut forged_unchanged = genuine.clone();
        forged_unchanged.unchanged.push(crate::identity::UnchangedFile {
            rel_path: "user.md".to_owned(),
            identity: FileIdentity::of(b"x"),
        });
        for forged in [forged_old, forged_path, forged_extra, forged_unchanged] {
            forged.validate().unwrap();
            assert_eq!(
                verify_with(dir.path(), &forged, &accept).unwrap_err().code,
                ErrorCode::JournalInvalid
            );
        }
    }

    #[test]
    fn a_missing_unprotected_altered_or_foreign_seal_is_an_error() {
        let dir = TempDir::new("seal-tamper");
        let install = install_id("/x");
        let transaction = TransactionId::generate().unwrap();
        let sealed = journal(&install, &transaction);
        assert_eq!(
            verify_with(dir.path(), &sealed, &accept).unwrap_err().code,
            ErrorCode::NotFound
        );
        write_with(dir.path(), &sealed, &accept).unwrap();
        assert!(verify_with(dir.path(), &sealed, &reject).is_err());

        let path = seal_path(dir.path(), &install, &transaction);
        let digest = sealed.recovery_digest().unwrap();
        for damaged in [
            "garbage\n".to_owned(),
            format!("{MAGIC}\n{}\n{}\nnot-a-hash\n", install.as_str(), transaction.as_str()),
            format!("{MAGIC}\n{}\n{}\n{digest}\nextra\n", install.as_str(), transaction.as_str()),
        ] {
            fs::write(&path, damaged).unwrap();
            assert!(verify_with(dir.path(), &sealed, &accept).is_err());
        }

        // A seal copied from another transaction is refused by its content.
        let other = TransactionId::generate().unwrap();
        let other_journal = journal(&install, &other);
        write_with(dir.path(), &other_journal, &accept).unwrap();
        fs::copy(
            seal_path(dir.path(), &install, &other),
            seal_path(dir.path(), &install, &transaction),
        )
        .unwrap();
        assert_eq!(
            verify_with(dir.path(), &sealed, &accept).unwrap_err().code,
            ErrorCode::IdentityMismatch
        );
    }

    #[test]
    fn a_failed_protection_check_publishes_nothing_and_a_seal_is_not_replaced() {
        let dir = TempDir::new("seal-write-fail");
        let install = install_id("/x");
        let transaction = TransactionId::generate().unwrap();
        let sealed = journal(&install, &transaction);
        assert!(write_with(dir.path(), &sealed, &reject).is_err());
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0, "no seal or temp file stays");

        write_with(dir.path(), &sealed, &accept).unwrap();
        let mut other = sealed.clone();
        other.steps[0].old = Some(FileIdentity::of(b"other"));
        assert!(write_with(dir.path(), &other, &accept).is_err());
        verify_with(dir.path(), &sealed, &accept).unwrap();
    }

    #[test]
    fn only_a_transaction_with_a_helper_identity_record_needs_a_seal() {
        let dir = TempDir::new("seal-required");
        let install = install_id("/x");
        let transaction = TransactionId::generate().unwrap();
        assert!(!required_in(dir.path(), &install, &transaction).unwrap());
        crate::helper_anchor::write_with(
            dir.path(),
            &install,
            &transaction,
            &FileIdentity::of(b"old helper"),
            &accept,
        )
        .unwrap();
        assert!(required_in(dir.path(), &install, &transaction).unwrap());
        write_with(dir.path(), &journal(&install, &transaction), &accept).unwrap();
        retire_in(dir.path(), &install, &transaction).unwrap();
        assert!(!seal_path(dir.path(), &install, &transaction).exists());
        retire_in(dir.path(), &install, &transaction).unwrap();
    }
}
