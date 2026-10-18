//! OS adapters for the update flow (Issue #510, U4 increment 1).
//!
//! This stage covers what must exist before any file is replaced: confirming
//! that an install is a supported placement, process-lifetime leases, and the
//! verified current-version helper copy. Windows is portable x64 on a local
//! fixed NTFS volume; macOS is a Developer ID `.app` bundle on APFS. Every
//! other placement is `UnsupportedInstall` (fail closed); ordinary editing
//! still works there, it just never offers an update.
//!
//! The Windows file-set replacement lives in `windows_apply` and the macOS
//! bundle exchange in `macos_apply`; both are driven by the helper, and the
//! limited-health child and durable commit are in `health`.

use std::fs;
use std::path::{Path, PathBuf};

use crate::error::{ErrorCode, Result, UpdateError};
use crate::fsutil::{durable_write, hash_regular_file, read_limited};
use crate::identity::{FileIdentity, InstallId, TransactionId};
use crate::journal::{DiskView, FsDiskView};
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

impl Placement {
    /// How the journal's relative paths are compared with this install's disk:
    /// owned files for Windows, the whole bundle (`Hane.app`) for macOS. A
    /// receipt of one layout is never read through the other's view.
    pub fn disk_view(&self) -> Box<dyn DiskView> {
        match self.target {
            Target::MacosArm64 => Box::new(crate::macos_apply::BundleView::new(&self.install_root)),
            Target::WindowsX64 => Box::new(FsDiskView::new(&self.install_root)),
        }
    }
}

pub(crate) fn unsupported(detail: impl Into<String>) -> UpdateError {
    UpdateError::new(ErrorCode::UnsupportedInstall, detail)
}

#[cfg(any(target_os = "windows", target_os = "macos"))]
pub(crate) fn owner_name() -> Result<String> {
    // The OS principal, never `USERNAME`/`USER`: the launching environment
    // cannot change it, and the helper (whose environment is empty) re-derives
    // the same value itself. An unavailable principal is a refusal, never a
    // placeholder that derives another InstallId.
    owner_from(acl::principal_id())
}

#[cfg(any(target_os = "windows", target_os = "macos", test))]
pub(crate) fn owner_from(principal: Result<String>) -> Result<String> {
    // A failed or empty principal is never `UnsupportedInstall`: startup treats
    // that as `Unmanaged` and would skip the recovery gate of a managed install
    // that has a pending journal. It is reported as `Io` (keeping the original
    // cause), which the gate turns into `RecoveryRequired`.
    match principal {
        Ok(owner) if !owner.is_empty() => Ok(owner),
        Ok(_) => Err(UpdateError::new(
            ErrorCode::Io,
            "the OS principal is empty",
        )),
        Err(err) => Err(UpdateError::new(
            ErrorCode::Io,
            format!("the OS principal is unavailable: {err}"),
        )),
    }
}

/// The install/bundle root must belong to the principal its `InstallId` is
/// derived from. Otherwise a root owned by someone else (reached through a
/// shared parent) would be updated, or checked through a coordination path of
/// another `InstallId`, with a pending journal overlooked. A mismatch or an
/// unreadable owner is never `UnsupportedInstall`, so startup stops instead of
/// treating the install as unmanaged.
#[cfg(any(target_os = "windows", target_os = "macos", test))]
pub(crate) fn require_root_owner(owner: Result<String>, principal: Result<String>) -> Result<()> {
    let principal = owner_from(principal)?;
    let owner = owner_from(owner)?;
    if owner == principal {
        Ok(())
    } else {
        Err(UpdateError::new(
            ErrorCode::IdentityMismatch,
            "the install root is not owned by the current user",
        ))
    }
}

#[cfg(any(target_os = "windows", target_os = "macos"))]
pub(crate) fn require_install_root_owner(root: &Path) -> Result<()> {
    require_root_owner(acl::directory_owner_id(root), acl::principal_id())
}

#[cfg(test)]
mod root_owner_tests {
    use super::*;

    #[test]
    fn a_root_owned_by_the_current_principal_continues() {
        require_root_owner(Ok("uid:501".into()), Ok("uid:501".into())).unwrap();
        require_root_owner(Ok("sid:0102".into()), Ok("sid:0102".into())).unwrap();
    }

    #[test]
    fn a_root_owned_by_another_principal_is_refused_without_being_unmanaged() {
        for (owner, principal) in [("uid:502", "uid:501"), ("sid:0a", "sid:0b")] {
            let err = require_root_owner(Ok(owner.into()), Ok(principal.into())).unwrap_err();
            assert_eq!(err.code, ErrorCode::IdentityMismatch);
        }
    }

    #[test]
    fn an_unreadable_owner_or_principal_is_refused_without_being_unmanaged() {
        let err = require_root_owner(Err(unsupported("no owner")), Ok("uid:1".into())).unwrap_err();
        assert_eq!(err.code, ErrorCode::Io);
        let err = require_root_owner(Ok("uid:1".into()), Err(unsupported("no token"))).unwrap_err();
        assert_eq!(err.code, ErrorCode::Io);
        let err = require_root_owner(Ok(String::new()), Ok("uid:1".into())).unwrap_err();
        assert_eq!(err.code, ErrorCode::Io);
    }

    #[cfg(unix)]
    #[test]
    fn a_real_directory_is_owned_by_this_user_and_a_link_is_refused() {
        let dir = crate::testing::TempDir::new("root-owner");
        require_root_owner(acl::directory_owner_id(dir.path()), acl::principal_id()).unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(dir.path(), &link).unwrap();
        assert!(acl::directory_owner_id(&link).is_err());
        assert!(acl::directory_owner_id(&dir.path().join("absent")).is_err());
    }
}

#[cfg(all(test, unix))]
mod owner_tests {
    use super::*;

    #[test]
    fn the_owner_is_the_os_principal_not_the_environment() {
        let owner = owner_from(acl::principal_id()).unwrap();
        assert_eq!(owner, acl::principal_id().unwrap());
        assert!(owner.starts_with("uid:"));
        for name in ["USERNAME", "USER"] {
            if let Ok(value) = std::env::var(name) {
                assert_ne!(owner, value);
            }
        }
    }

    #[test]
    fn an_unavailable_principal_is_not_unsupported() {
        let err = owner_from(Err(unsupported("no token"))).unwrap_err();
        assert_eq!(err.code, ErrorCode::Io);
        assert!(err.detail.contains("no token"));
        let err = owner_from(Ok(String::new())).unwrap_err();
        assert_eq!(err.code, ErrorCode::Io);
    }
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
        // An interrupted update can leave the managed files incomplete, and a
        // moved install derives another `InstallId`. The relocation record,
        // written before the first mutation, still names the original id.
        let path = entry.path();
        if let Ok(Some(record)) = crate::relocation::read(&path)
            && &record.install_id == coordination.install_id()
        {
            return Ok(Placement {
                target: Target::WindowsX64,
                install_root: path,
                coordination_parent: parent.to_path_buf(),
                install_id: record.install_id,
            });
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
        // Recovery after a crash must not trust the live install, which may be
        // half replaced. It runs this copy, bound to the identity recorded here.
        if let Err(err) = write_helper_record(coordination, transaction, &source.identity, &dest) {
            let _ = fs::remove_file(&dest);
            return Err(err);
        }
        Ok(dest)
    } else {
        let _ = fs::remove_file(&dest);
        Err(UpdateError::new(
            ErrorCode::HashMismatch,
            "the helper copy differs from the verified helper",
        ))
    }
}

const HELPER_RECORD_FILE: &str = "helper.identity";
const HELPER_RECORD_LIMIT: u64 = 512;

fn helper_record_path(coordination: &Coordination, transaction: &TransactionId) -> PathBuf {
    coordination.transaction_dir(transaction).join(HELPER_RECORD_FILE)
}

fn record_text(
    coordination: &Coordination,
    transaction: &TransactionId,
    identity: &FileIdentity,
) -> String {
    format!(
        "{}\n{}\n{}\n{}\n",
        coordination.install_id().as_str(),
        transaction.as_str(),
        identity.size,
        identity.sha256
    )
}

/// Records which install, transaction and bytes the staged helper copy was
/// verified as, in the owner-only transaction directory.
fn write_helper_record(
    coordination: &Coordination,
    transaction: &TransactionId,
    identity: &FileIdentity,
    staged: &Path,
) -> Result<()> {
    let record = helper_record_path(coordination, transaction);
    // The copy must still be exactly what was verified before it is vouched for.
    if hash_regular_file(staged)?.map(|(size, sha256)| FileIdentity { size, sha256 }).as_ref()
        != Some(identity)
    {
        return Err(UpdateError::new(
            ErrorCode::HashMismatch,
            "the helper copy changed before it was recorded",
        ));
    }
    durable_write(&record, record_text(coordination, transaction, identity).as_bytes())?;
    if let Err(err) = acl::secure_new(&record, false, 0o600).and_then(|()| acl::verify(&record, false)) {
        let _ = fs::remove_file(&record);
        return Err(err);
    }
    Ok(())
}

/// The helper copy staged for `transaction` before any mutation, re-verified
/// against the identity recorded then. It is the only helper pending-transaction
/// recovery may run: the live install may be half replaced after a power loss.
/// A missing, altered or foreign copy or record is an error, never a fallback.
pub fn staged_helper(coordination: &Coordination, transaction: &TransactionId) -> Result<PathBuf> {
    let refuse = |detail: &str| UpdateError::new(ErrorCode::IdentityMismatch, detail.to_owned());
    let record = helper_record_path(coordination, transaction);
    let raw = read_limited(&record, HELPER_RECORD_LIMIT)?
        .ok_or_else(|| UpdateError::new(ErrorCode::NotFound, "the staged helper record is missing"))?;
    acl::verify(&record, false)?;
    let text = String::from_utf8(raw).map_err(|_| refuse("the staged helper record is malformed"))?;
    let mut lines = text.lines();
    let (Some(install), Some(txn), Some(size), Some(sha256), None) =
        (lines.next(), lines.next(), lines.next(), lines.next(), lines.next())
    else {
        return Err(refuse("the staged helper record is malformed"));
    };
    if install != coordination.install_id().as_str() || txn != transaction.as_str() {
        return Err(refuse("the staged helper record belongs to another install or transaction"));
    }
    let identity = FileIdentity {
        size: size.parse().map_err(|_| refuse("the staged helper record is malformed"))?,
        sha256: sha256.to_owned(),
    };
    let staged = coordination
        .transaction_dir(transaction)
        .join(lease::HELPER_DIR)
        .join(HELPER_FILE_NAME);
    let present = hash_regular_file(&staged)?
        .ok_or_else(|| UpdateError::new(ErrorCode::NotFound, "the staged helper is missing"))?;
    if (FileIdentity { size: present.0, sha256: present.1 }) != identity {
        return Err(UpdateError::new(
            ErrorCode::HashMismatch,
            "the staged helper differs from its recorded identity",
        ));
    }
    acl::verify(&staged, false)?;
    Ok(staged)
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
