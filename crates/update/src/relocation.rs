//! The relocation record of a Windows update (Issue #510).
//!
//! The `InstallId` of a Windows install is derived from where it lives, and the
//! placement check needs every managed file. After an interrupted update, or
//! after the install (or a parent) was moved, neither can be relied on to find
//! the original transaction. Before the first install mutation the update
//! therefore writes this record into the install root, outside the signed
//! inventory: it binds the original `InstallId`, the `TransactionId` and the
//! coordination parent, so startup recovery can still reach the original journal
//! and its staged helper, or else stop. The record is owner-only; one that
//! cannot be read or verified is an error, never "no record".

use std::fs;
use std::path::{Path, PathBuf};

use crate::error::{ErrorCode, Result, UpdateError};
use crate::fsutil::{durable_write, read_limited};
use crate::identity::{InstallId, TransactionId};
use crate::platform::acl;
use crate::platform::lease::Coordination;

/// File name inside the install root; it is not listed by any inventory.
pub const RELOCATION_FILE: &str = "hane-update-relocation";
const MAGIC: &str = "hane-update-relocation-v1";
const RECORD_LIMIT: u64 = 4096;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelocationRecord {
    pub install_id: InstallId,
    pub transaction: TransactionId,
    pub coordination_parent: PathBuf,
}

fn malformed() -> UpdateError {
    UpdateError::new(ErrorCode::IdentityMismatch, "the relocation record is malformed")
}

fn record_path(install_root: &Path) -> PathBuf {
    install_root.join(RELOCATION_FILE)
}

/// Durably records the original identities of `transaction`. Called after the
/// journal exists and before the first install mutation.
pub fn write(
    install_root: &Path,
    coordination: &Coordination,
    transaction: &TransactionId,
) -> Result<()> {
    let parent = coordination
        .dir()
        .parent()
        .and_then(Path::to_str)
        .ok_or_else(|| UpdateError::new(ErrorCode::Io, "the coordination parent is not UTF-8"))?;
    let text = format!(
        "{MAGIC}\n{}\n{}\n{parent}\n",
        coordination.install_id().as_str(),
        transaction.as_str()
    );
    let path = record_path(install_root);
    durable_write(&path, text.as_bytes())?;
    if let Err(err) = acl::secure_new(&path, false, 0o600).and_then(|()| acl::verify(&path, false)) {
        let _ = fs::remove_file(&path);
        return Err(err);
    }
    Ok(())
}

/// The record of `install_root`; `None` only when there is definitely none.
pub fn read(install_root: &Path) -> Result<Option<RelocationRecord>> {
    let path = record_path(install_root);
    let Some(raw) = read_limited(&path, RECORD_LIMIT)? else {
        return Ok(None);
    };
    acl::verify(&path, false)?;
    let text = String::from_utf8(raw).map_err(|_| malformed())?;
    let mut lines = text.lines();
    let (Some(magic), Some(install), Some(txn), Some(parent), None) =
        (lines.next(), lines.next(), lines.next(), lines.next(), lines.next())
    else {
        return Err(malformed());
    };
    if magic != MAGIC || parent.is_empty() {
        return Err(malformed());
    }
    Ok(Some(RelocationRecord {
        install_id: InstallId::parse(install).map_err(|_| malformed())?,
        transaction: TransactionId::parse(txn).map_err(|_| malformed())?,
        coordination_parent: PathBuf::from(parent),
    }))
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

    #[test]
    fn a_record_round_trips_and_absence_is_none() {
        let dir = TempDir::new("relocation-record");
        let root = dir.path().join("Hane");
        fs::create_dir_all(&root).unwrap();
        assert_eq!(read(&root).unwrap(), None);
        let coordination = Coordination::create(dir.path(), &install_id()).unwrap();
        let transaction = TransactionId::generate().unwrap();
        write(&root, &coordination, &transaction).unwrap();
        let record = read(&root).unwrap().unwrap();
        assert_eq!(record.install_id, install_id());
        assert_eq!(record.transaction, transaction);
        assert_eq!(record.coordination_parent, dir.path());
    }

    #[test]
    fn a_damaged_record_is_an_error_not_an_absence() {
        let dir = TempDir::new("relocation-damaged");
        let root = dir.path().join("Hane");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join(RELOCATION_FILE), b"garbage\n").unwrap();
        assert!(read(&root).is_err());
    }
}
