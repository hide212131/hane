//! OS adapters for the update flow (Issue #510, U4 increment 1).
//!
//! This stage covers what must exist before any file is replaced: confirming
//! that an install is a supported placement, process-lifetime leases, and the
//! verified current-version helper copy. Windows is portable x64 on a local
//! fixed NTFS volume; macOS is a Developer ID `.app` bundle on APFS. Every
//! other placement is `UnsupportedInstall` (fail closed); ordinary editing
//! still works there, it just never offers an update.
//!
//! The Windows file-set replacement lives in `windows_apply` and is driven by
//! the helper. Not here yet: macOS `RENAME_SWAP`, the limited-health child and
//! the durable commit.

use std::fs;
use std::path::{Path, PathBuf};

use crate::error::{ErrorCode, Result, UpdateError};
use crate::fsutil::{durable_write, hash_regular_file, read_limited};
use crate::identity::{FileIdentity, InstallId, TransactionId};
use crate::limits::MAX_ENTRY_BYTES;
use crate::model::Target;

pub(crate) mod acl;
pub mod lease;
#[cfg(target_os = "macos")]
pub mod macos;
#[cfg(target_os = "windows")]
pub mod windows;

use lease::Coordination;

#[cfg(windows)]
pub const HELPER_FILE_NAME: &str = "hane-updater.exe";
#[cfg(not(windows))]
pub const HELPER_FILE_NAME: &str = "hane-updater";

/// A supported, verified installation of the running application.
#[derive(Debug, Clone)]
pub struct Placement {
    pub target: Target,
    /// The directory holding `hane.exe`, or the `.app` bundle.
    pub install_root: PathBuf,
    /// Where `.hane-update-<InstallId>/` lives (the parent of the install).
    pub coordination_parent: PathBuf,
    pub install_id: InstallId,
}

impl Placement {
    pub fn coordination(&self) -> Result<Coordination> {
        Coordination::create(&self.coordination_parent, &self.install_id)
    }

    pub fn existing_coordination(&self) -> Result<Option<Coordination>> {
        Coordination::open_existing(&self.coordination_parent, &self.install_id)
    }
}

/// The signed/owned helper in the current install and the identity it must have.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HelperSource {
    pub path: PathBuf,
    pub identity: FileIdentity,
}

pub(crate) fn unsupported(detail: impl Into<String>) -> UpdateError {
    UpdateError::new(ErrorCode::UnsupportedInstall, detail)
}

#[cfg(any(target_os = "windows", target_os = "macos"))]
pub(crate) fn owner_name() -> Result<String> {
    // The application hands this exact value to the helper (see
    // `helper::spawn_helper`), whose environment is otherwise empty. A missing
    // owner is a refusal, never a placeholder that derives another InstallId.
    std::env::var("USERNAME")
        .or_else(|_| std::env::var("USER"))
        .ok()
        .filter(|owner| !owner.is_empty())
        .ok_or_else(|| unsupported("the OS user name is unavailable"))
}

/// Confirms that `exe` is the running application of a supported install.
#[cfg(target_os = "windows")]
pub fn locate(exe: &Path) -> Result<Placement> {
    windows::locate(exe)
}

#[cfg(target_os = "macos")]
pub fn locate(exe: &Path) -> Result<Placement> {
    macos::locate(exe)
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
pub fn locate(exe: &Path) -> Result<Placement> {
    let _ = exe;
    Err(unsupported("this OS has no supported update placement"))
}

fn candidate_exe(dir: &Path) -> PathBuf {
    if cfg!(target_os = "macos") {
        dir.join("Contents").join("MacOS").join("hane")
    } else if cfg!(windows) {
        dir.join("hane.exe")
    } else {
        dir.join("hane")
    }
}

/// Finds the installation a coordination directory belongs to without being
/// told any path: only a sibling whose own verified `InstallId` equals the
/// coordination directory's is accepted.
pub fn find_install(coordination: &Coordination) -> Result<Placement> {
    let parent = coordination
        .dir()
        .parent()
        .ok_or_else(|| unsupported("the coordination directory has no parent"))?;
    for entry in fs::read_dir(parent)?.flatten() {
        if let Ok(placement) = locate(&candidate_exe(&entry.path()))
            && &placement.install_id == coordination.install_id()
        {
            return Ok(placement);
        }
    }
    Err(UpdateError::new(
        ErrorCode::NotFound,
        "no installation matches this coordination directory",
    ))
}

/// Verifies the installed helper against the current version's signed facts.
#[cfg(target_os = "windows")]
pub fn installed_helper(
    placement: &Placement,
    policy: &crate::trust::TrustPolicy,
) -> Result<HelperSource> {
    windows::installed_helper(placement, policy)
}

#[cfg(target_os = "macos")]
pub fn installed_helper(
    placement: &Placement,
    policy: &crate::trust::TrustPolicy,
) -> Result<HelperSource> {
    macos::installed_helper(placement, policy)
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
pub fn installed_helper(
    placement: &Placement,
    policy: &crate::trust::TrustPolicy,
) -> Result<HelperSource> {
    let _ = (placement, policy);
    Err(unsupported("this OS has no supported update placement"))
}

/// Copies the verified helper to `private/<TransactionId>/helper/` and
/// re-verifies the copy. The copy, never the file inside the install, is run,
/// so the install can be replaced while the helper is alive.
pub fn stage_helper_copy(
    coordination: &Coordination,
    transaction: &TransactionId,
    source: &HelperSource,
) -> Result<PathBuf> {
    let bytes = read_limited(&source.path, MAX_ENTRY_BYTES)?.ok_or_else(|| {
        UpdateError::new(ErrorCode::NotFound, "the installed helper is missing")
    })?;
    if FileIdentity::of(&bytes) != source.identity {
        return Err(UpdateError::new(
            ErrorCode::HashMismatch,
            "the installed helper differs from the signed identity",
        ));
    }
    let dir = coordination.ensure_helper_dir(transaction)?;
    let dest = dir.join(HELPER_FILE_NAME);
    durable_write(&dest, &bytes)?;
    // The copy must be owned by this principal and writable by nobody else
    // before it is hashed and run; the private subtree keeps it that way.
    if let Err(err) = acl::secure_new(&dest, false, 0o700).and_then(|()| acl::verify(&dest, false)) {
        let _ = fs::remove_file(&dest);
        return Err(err);
    }
    let copied = hash_regular_file(&dest)?.map(|(size, sha256)| FileIdentity { size, sha256 });
    if copied.as_ref() == Some(&source.identity) {
        Ok(dest)
    } else {
        let _ = fs::remove_file(&dest);
        Err(UpdateError::new(
            ErrorCode::HashMismatch,
            "the helper copy differs from the verified helper",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::InstallFacts;
    use crate::testing::TempDir;

    fn coordination(dir: &TempDir) -> Coordination {
        let id = InstallId::derive(&InstallFacts {
            os: "test",
            volume_id: "v",
            install_path: "/x",
            owner: "u",
        })
        .unwrap();
        Coordination::create(dir.path(), &id).unwrap()
    }

    #[test]
    fn the_helper_copy_is_verified_before_and_after_copying() {
        let dir = TempDir::new("helper-copy");
        let coord = coordination(&dir);
        let txn = TransactionId::generate().unwrap();
        let source = dir.path().join("hane-updater-source");
        fs::write(&source, b"helper-bytes").unwrap();

        let good = HelperSource {
            path: source.clone(),
            identity: FileIdentity::of(b"helper-bytes"),
        };
        let copy = stage_helper_copy(&coord, &txn, &good).unwrap();
        assert_eq!(fs::read(&copy).unwrap(), b"helper-bytes");
        assert!(
            Coordination::from_helper_exe(&copy).is_ok(),
            "the copy lives where the helper derives its identity from"
        );

        let bad = HelperSource {
            path: source,
            identity: FileIdentity::of(b"something-else"),
        };
        let other_txn = TransactionId::generate().unwrap();
        let err = stage_helper_copy(&coord, &other_txn, &bad).unwrap_err();
        assert_eq!(err.code, ErrorCode::HashMismatch);
        assert!(!coord.transaction_dir(&other_txn).join("helper").join(HELPER_FILE_NAME).exists());
    }

    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    #[test]
    fn unsupported_placements_fail_closed() {
        let err = locate(Path::new("/opt/hane/hane")).unwrap_err();
        assert_eq!(err.code, ErrorCode::UnsupportedInstall);
    }
}
