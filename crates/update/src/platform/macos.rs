//! macOS placement checks: a Developer ID signed `Hane.app` on a local,
//! writable APFS volume, running from its normal location.
//!
//! Two `unsafe` spots live here: the read-only volume file-system query and
//! the `renamex_np(RENAME_SWAP)` exchange of the bundle-swap increment.
//! Signature checking and the Gatekeeper assessment are delegated to the OS
//! `codesign` and `spctl` tools with fixed arguments (no shell); the catalog
//! signature and Apple's signature are separate checks and neither replaces the
//! other. [`MacHost`] is the real [`BundleHost`] behind the
//! `crate::macos_apply` bundle swap; it never falls back to a two-step rename.
//! None of this has run against a real signed candidate yet (U2 / U5).

#![allow(
    unsafe_code,
    reason = "read-only statfs query and renamex_np(RENAME_SWAP) through libc"
)]

use std::ffi::CString;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use super::{HELPER_FILE_NAME, HelperSource, Placement, owner_name, unsupported};
use crate::error::{ErrorCode, Result, UpdateError};
use crate::fsutil::{hash_regular_file, read_limited};
use crate::identity::{FileIdentity, InstallFacts, InstallId};
use crate::macos_apply::{
    BundleHost, VolumeFacts, plist_string, required_team_id, valid_team_id,
};
use crate::model::{MACOS_BUNDLE_ID, Target};
use crate::trust::TrustPolicy;

const CODESIGN: &str = "/usr/bin/codesign";
const SPCTL: &str = "/usr/sbin/spctl";
const MAIN_EXECUTABLE: &str = "hane";
const MAX_PLIST_BYTES: u64 = 256 << 10;
/// `RENAME_SWAP` of `<sys/stdio.h>`: exchange the two names atomically.
const RENAME_SWAP: libc::c_uint = 0x2;

unsafe extern "C" {
    fn renamex_np(
        from: *const libc::c_char,
        to: *const libc::c_char,
        flags: libc::c_uint,
    ) -> libc::c_int;
}

fn query_volume(path: &Path) -> Result<VolumeFacts> {
    let c_path = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| unsupported("the install path contains a NUL byte"))?;
    // SAFETY: `statfs` is plain data, so the zeroed value is valid storage; the
    // path is NUL-terminated and the out-pointer refers to that local.
    let (rc, stat) = unsafe {
        let mut stat: libc::statfs = std::mem::zeroed();
        let rc = libc::statfs(c_path.as_ptr(), &mut stat);
        (rc, stat)
    };
    if rc != 0 {
        return Err(UpdateError::new(
            ErrorCode::Io,
            "the install volume could not be queried",
        ));
    }
    let name_bytes: Vec<u8> = stat
        .f_fstypename
        .iter()
        .take_while(|c| **c != 0)
        .map(|c| *c as u8)
        .collect();
    let flags = i64::from(stat.f_flags);
    Ok(VolumeFacts {
        id: format!("{:x}", fs::metadata(path)?.dev()),
        apfs: name_bytes == b"apfs",
        local: flags & i64::from(libc::MNT_LOCAL) != 0,
        read_only: flags & i64::from(libc::MNT_RDONLY) != 0,
    })
}

/// Bytes an unprivileged writer can still allocate on the volume of `path`
/// (`f_bavail * f_bsize`). A failed query or an overflow is an error.
fn query_available_bytes(path: &Path) -> Result<u64> {
    let failed = || UpdateError::new(ErrorCode::Io, "the free space of the install volume could not be queried");
    let c_path = CString::new(path.as_os_str().as_bytes()).map_err(|_| failed())?;
    // SAFETY: `statfs` is plain data, so the zeroed value is valid storage; the
    // path is NUL-terminated and the out-pointer refers to that local.
    let (rc, stat) = unsafe {
        let mut stat: libc::statfs = std::mem::zeroed();
        let rc = libc::statfs(c_path.as_ptr(), &mut stat);
        (rc, stat)
    };
    if rc != 0 {
        return Err(failed());
    }
    stat.f_bavail
        .checked_mul(u64::from(stat.f_bsize))
        .ok_or_else(|| UpdateError::new(ErrorCode::LimitExceeded, "the free space of the install volume overflows"))
}

/// `struct attrlist` of `<sys/attr.h>`.
#[repr(C)]
struct AttrList {
    bitmapcount: u16,
    reserved: u16,
    commonattr: u32,
    volattr: u32,
    dirattr: u32,
    fileattr: u32,
    forkattr: u32,
}

const ATTR_BIT_MAP_COUNT: u16 = 5;
const ATTR_VOL_INFO: u32 = 0x8000_0000;
const ATTR_VOL_UUID: u32 = 0x0004_0000;

unsafe extern "C" {
    fn getattrlist(
        path: *const libc::c_char,
        attr_list: *mut AttrList,
        attr_buf: *mut libc::c_void,
        attr_buf_size: libc::size_t,
        options: libc::c_ulong,
    ) -> libc::c_int;
}

/// The persistent UUID of the volume holding `path`, lowercase hex. Any
/// failure (including an all-zero UUID) is an error, never a fallback value.
fn volume_uuid(path: &Path) -> Result<String> {
    let failed = || {
        UpdateError::new(
            ErrorCode::Io,
            "the install volume UUID could not be determined",
        )
    };
    let c_path = CString::new(path.as_os_str().as_bytes()).map_err(|_| failed())?;
    let mut list = AttrList {
        bitmapcount: ATTR_BIT_MAP_COUNT,
        reserved: 0,
        commonattr: 0,
        volattr: ATTR_VOL_INFO | ATTR_VOL_UUID,
        dirattr: 0,
        fileattr: 0,
        forkattr: 0,
    };
    // 4-byte length prefix followed by the 16-byte uuid_t.
    let mut buf = [0u8; 32];
    // SAFETY: the path is NUL-terminated, `list` and `buf` are live locals and
    // the size passed is that of `buf`; the call only writes within it.
    let rc = unsafe {
        getattrlist(
            c_path.as_ptr(),
            &mut list,
            buf.as_mut_ptr().cast(),
            buf.len(),
            0,
        )
    };
    if rc != 0 {
        return Err(failed());
    }
    let length = u32::from_ne_bytes([buf[0], buf[1], buf[2], buf[3]]);
    if length < 20 {
        return Err(failed());
    }
    uuid_to_hex(&buf[4..20]).ok_or_else(failed)
}

fn uuid_to_hex(bytes: &[u8]) -> Option<String> {
    if bytes.len() != 16 || bytes.iter().all(|b| *b == 0) {
        return None;
    }
    Some(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// The InstallId of a bundle on `volume`. Only the persistent volume UUID
/// enters the identity; `volume.id` (st_dev) can change when the volume is
/// remounted. A missing UUID is an error so startup fails closed.
fn derive_install_id(
    volume: &VolumeFacts,
    uuid: Result<String>,
    install_path: &str,
    owner: &str,
) -> Result<InstallId> {
    let _ = &volume.id;
    let uuid = uuid?;
    InstallId::derive(&InstallFacts {
        os: "macos",
        volume_id: &format!("apfs-uuid:{uuid}"),
        install_path,
        owner,
    })
}

pub fn locate(exe: &Path) -> Result<Placement> {
    if Target::current()? != Target::MacosArm64 {
        return Err(unsupported("not an Apple Silicon macOS build"));
    }
    let exe = fs::canonicalize(exe)?;
    // <bundle>.app/Contents/MacOS/hane
    let macos_dir = exe.parent().ok_or_else(|| unsupported("not inside an app bundle"))?;
    let contents = macos_dir.parent().ok_or_else(|| unsupported("not inside an app bundle"))?;
    let bundle = contents.parent().ok_or_else(|| unsupported("not inside an app bundle"))?;
    let layout_ok = exe.file_name().is_some_and(|n| n == MAIN_EXECUTABLE)
        && macos_dir.file_name().is_some_and(|n| n == "MacOS")
        && contents.file_name().is_some_and(|n| n == "Contents")
        && bundle.extension().is_some_and(|ext| ext == "app");
    if !layout_ok {
        return Err(unsupported("a bare binary outside Hane.app is not updatable"));
    }
    if bundle.components().any(|c| c.as_os_str() == "AppTranslocation") {
        return Err(unsupported("move the app out of App Translocation first"));
    }
    let coordination_parent = bundle
        .parent()
        .ok_or_else(|| unsupported("the bundle has no parent for coordination"))?
        .to_path_buf();
    // The layout is Hane.app's, so a missing or unreadable Info.plist is a
    // damaged bundle, not an unsupported placement.
    let plist = read_limited(&contents.join("Info.plist"), MAX_PLIST_BYTES)?.ok_or_else(|| {
        UpdateError::new(ErrorCode::InstallModified, "Info.plist is missing from the bundle")
    })?;
    let plist = String::from_utf8(plist).map_err(|_| {
        UpdateError::new(ErrorCode::InstallModified, "Info.plist is not XML")
    })?;
    if plist_string(&plist, "CFBundleIdentifier").as_deref() != Some(MACOS_BUNDLE_ID) {
        return Err(unsupported("the bundle identifier is not Hane's"));
    }
    let volume = query_volume(bundle)?;
    if !volume.apfs || !volume.local || volume.read_only {
        // A marked bundle may have been moved here in the middle of an update:
        // the original transaction is consulted before this counts as unmanaged.
        crate::macos_apply::check_relocation_on_unsupported_volume(
            read_mark(bundle)?.as_deref(),
            &coordination_parent,
        )?;
        return Err(if volume.read_only && volume.apfs && volume.local {
            unsupported("the install volume is read-only")
        } else {
            unsupported("only a local APFS volume is supported")
        });
    }
    super::require_install_root_owner(bundle)?;
    // `volume.id` (st_dev) only identifies the mount for this boot; the durable
    // InstallId uses the persistent volume UUID and fails closed without it.
    let install_id = derive_install_id(
        &volume,
        volume_uuid(bundle),
        crate::identity::install_path_text(bundle)?,
        &owner_name()?,
    )?;
    // A bundle swapped in by an update names the install it was swapped into.
    // If it was moved, `install_id` differs and the pending transaction of the
    // original location is consulted; unverifiable or pending stops (this is
    // `InstallModified`/`Io`, never `UnsupportedInstall`, so it is not unmanaged).
    crate::macos_apply::check_relocation(
        read_mark(bundle)?.as_deref(),
        &install_id,
        &coordination_parent,
    )?;
    Ok(Placement {
        target: Target::MacosArm64,
        install_root: bundle.to_path_buf(),
        coordination_parent,
        install_id,
    })
}

/// Asks the OS to verify the complete bundle against the Developer ID
/// requirement pinned to exactly `team_id` (leaf certificate OU).
pub fn verify_developer_id(bundle: &Path, team_id: &str) -> Result<()> {
    if !valid_team_id(team_id) {
        return Err(UpdateError::new(ErrorCode::IdentityMismatch, "malformed Team ID"));
    }
    let requirement = format!(
        "anchor apple generic and certificate 1[field.1.2.840.113635.100.6.2.6] exists \
         and certificate leaf[field.1.2.840.113635.100.6.1.13] exists \
         and certificate leaf[subject.OU] = \"{team_id}\""
    );
    let status = Command::new(CODESIGN)
        .arg("--verify")
        .arg("--deep")
        .arg("--strict")
        .arg(format!("--test-requirement={requirement}"))
        .arg(bundle)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(UpdateError::new(
            ErrorCode::SignatureInvalid,
            "the bundle is not validly signed with Developer ID",
        ))
    }
}

/// The helper inside the sealed bundle, accepted only if the bundle verifies
/// against the pinned Team ID. A build without one provides no helper recovery
/// (intended fail-closed behavior).
pub fn installed_helper(placement: &Placement, policy: &TrustPolicy) -> Result<HelperSource> {
    let team_id = required_team_id(policy)?;
    verify_developer_id(&placement.install_root, team_id)?;
    let path = placement
        .install_root
        .join("Contents")
        .join("Helpers")
        .join(HELPER_FILE_NAME);
    let (size, sha256) = hash_regular_file(&path)?.ok_or_else(|| {
        UpdateError::new(ErrorCode::NotFound, "the bundled helper is missing")
    })?;
    Ok(HelperSource {
        path,
        identity: FileIdentity { size, sha256 },
    })
}

fn c_path(path: &Path) -> Result<CString> {
    CString::new(path.as_os_str().as_bytes())
        .map_err(|_| unsupported("a bundle path contains a NUL byte"))
}

/// Atomically exchanges two existing paths on one volume. There is no
/// fallback: if the call fails, nothing has changed.
fn exchange(first: &Path, second: &Path) -> Result<()> {
    let (first, second) = (c_path(first)?, c_path(second)?);
    // SAFETY: both pointers refer to NUL-terminated C strings that outlive the
    // call, and `renamex_np` keeps neither.
    let rc = unsafe { renamex_np(first.as_ptr(), second.as_ptr(), RENAME_SWAP) };
    if rc == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error().into())
    }
}

/// The extended attribute carrying the relocation mark of a bundle.
const MARK_XATTR: &std::ffi::CStr = c"io.github.hide212131.hane.update-mark";
/// A mark is a few short lines; anything larger is damaged.
const MAX_MARK_BYTES: usize = 4096;

/// The relocation mark of `bundle`, `None` only if the attribute definitely
/// does not exist. Every other failure is an error so startup stops.
fn read_mark(bundle: &Path) -> Result<Option<Vec<u8>>> {
    let path = c_path(bundle)?;
    let mut buf = vec![0u8; MAX_MARK_BYTES + 1];
    // SAFETY: the path and the name are NUL-terminated C strings; the buffer
    // and its length describe `buf`, which the call writes within.
    let size = unsafe {
        libc::getxattr(
            path.as_ptr(),
            MARK_XATTR.as_ptr(),
            buf.as_mut_ptr().cast(),
            buf.len(),
            0,
            libc::XATTR_NOFOLLOW,
        )
    };
    if size < 0 {
        let err = std::io::Error::last_os_error();
        return if err.raw_os_error() == Some(libc::ENOATTR) {
            Ok(None)
        } else {
            Err(err.into())
        };
    }
    let size = size as usize;
    if size > MAX_MARK_BYTES {
        return Err(UpdateError::new(
            ErrorCode::InstallModified,
            "the bundle relocation mark is too large",
        ));
    }
    buf.truncate(size);
    Ok(Some(buf))
}

/// The real [`BundleHost`]: `statfs`, `renamex_np(RENAME_SWAP)`, `codesign`
/// and `spctl`, each with fixed arguments and no shell.
pub struct MacHost;

impl BundleHost for MacHost {
    fn volume(&self, path: &Path) -> Result<VolumeFacts> {
        query_volume(path)
    }

    fn available_bytes(&self, path: &Path) -> Result<u64> {
        query_available_bytes(path)
    }

    /// Proves the exchange between the two parents with scratch directories
    /// of their own, so the bundle itself is never used as a test subject.
    fn supports_swap(&self, first_dir: &Path, second_dir: &Path) -> Result<bool> {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or(0);
        let name = |side: &str| format!(".hane-swap-probe-{}-{stamp:x}-{side}", std::process::id());
        let first = first_dir.join(name("a"));
        let second = second_dir.join(name("b"));
        fs::create_dir(&first)?;
        if let Err(err) = fs::create_dir(&second) {
            let _ = fs::remove_dir(&first);
            return Err(err.into());
        }
        let swapped = fs::write(first.join("marker"), b"probe").is_ok()
            && exchange(&first, &second).is_ok()
            && second.join("marker").exists()
            && !first.join("marker").exists();
        for dir in [&first, &second] {
            let _ = fs::remove_file(dir.join("marker"));
            let _ = fs::remove_dir(dir);
        }
        Ok(swapped)
    }

    fn swap(&self, first: &Path, second: &Path) -> Result<()> {
        exchange(first, second)
    }

    /// `renamex_np` only updates the cached directory entries. `F_FULLFSYNC`
    /// on both parent directories (the live bundle's and the slot's) flushes
    /// the exchange to the device; a failure is an error, never a success.
    fn sync_swap(&self, first: &Path, second: &Path) -> Result<()> {
        use std::os::fd::AsRawFd;
        let mut parents = Vec::with_capacity(2);
        for path in [first, second] {
            let parent = path
                .parent()
                .ok_or_else(|| unsupported("a bundle path has no parent directory"))?;
            if !parents.contains(&parent) {
                parents.push(parent);
            }
        }
        for parent in parents {
            let dir = fs::File::open(parent)?;
            // SAFETY: the descriptor is open for the duration of the call.
            let rc = unsafe { libc::fcntl(dir.as_raw_fd(), libc::F_FULLFSYNC) };
            if rc != 0 {
                return Err(std::io::Error::last_os_error().into());
            }
        }
        Ok(())
    }

    fn verify_developer_id(&self, bundle: &Path, team_id: &str) -> Result<()> {
        verify_developer_id(bundle, team_id)
    }

    fn bundle_mark(&self, bundle: &Path) -> Result<Option<Vec<u8>>> {
        read_mark(bundle)
    }

    fn set_bundle_mark(&self, bundle: &Path, mark: &[u8]) -> Result<()> {
        let path = c_path(bundle)?;
        // SAFETY: the path and the attribute name are NUL-terminated C strings
        // and the value pointer and length describe `mark`; nothing is kept.
        let rc = unsafe {
            libc::setxattr(
                path.as_ptr(),
                MARK_XATTR.as_ptr(),
                mark.as_ptr().cast(),
                mark.len(),
                0,
                libc::XATTR_NOFOLLOW,
            )
        };
        if rc == 0 {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error().into())
        }
    }

    /// `setxattr` returning success only means the attribute is in the cache.
    /// `F_FULLFSYNC` on the bundle directory flushes its metadata to the
    /// device; a failure is an error, never a silent success.
    fn sync_bundle_mark(&self, bundle: &Path) -> Result<()> {
        use std::os::fd::AsRawFd;
        let dir = fs::File::open(bundle)?;
        // SAFETY: the descriptor is open for the duration of the call.
        let rc = unsafe { libc::fcntl(dir.as_raw_fd(), libc::F_FULLFSYNC) };
        if rc != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        if let Some(parent) = bundle.parent() {
            // The directory entry of the bundle itself.
            fs::File::open(parent)?.sync_all()?;
        }
        Ok(())
    }

    fn clear_bundle_mark(&self, bundle: &Path) -> Result<()> {
        let path = c_path(bundle)?;
        // SAFETY: both pointers refer to NUL-terminated C strings.
        let rc = unsafe { libc::removexattr(path.as_ptr(), MARK_XATTR.as_ptr(), libc::XATTR_NOFOLLOW) };
        if rc == 0 {
            return Ok(());
        }
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() == Some(libc::ENOATTR) {
            Ok(())
        } else {
            Err(err.into())
        }
    }

    fn assess_gatekeeper(&self, bundle: &Path) -> Result<()> {
        let status = Command::new(SPCTL)
            .args(["--assess", "--type", "execute"])
            .arg(bundle)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()?;
        if status.success() {
            Ok(())
        } else {
            Err(UpdateError::new(
                ErrorCode::SignatureInvalid,
                "Gatekeeper does not accept the bundle",
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::TempDir;

    #[test]
    fn the_bundle_identifier_is_read_from_an_xml_plist_only() {
        let plist = "<dict><key>CFBundleIdentifier</key>\n<string>io.github.hide212131.hane</string></dict>";
        assert_eq!(
            plist_string(plist, "CFBundleIdentifier").as_deref(),
            Some(MACOS_BUNDLE_ID)
        );
        assert_eq!(plist_string("bplist00\u{1}", "CFBundleIdentifier"), None);
    }

    #[test]
    fn team_ids_are_validated_before_they_reach_the_requirement() {
        assert!(valid_team_id("ABCDE12345"));
        assert!(!valid_team_id("abcde12345"));
        assert!(!valid_team_id("ABCDE\"1234"));
        let err = verify_developer_id(Path::new("/nonexistent"), "bad").unwrap_err();
        assert_eq!(err.code, ErrorCode::IdentityMismatch);
    }

    #[test]
    fn a_missing_team_id_is_rejected_before_any_os_command() {
        let policy = TrustPolicy::new(Vec::new(), None);
        let err = required_team_id(&policy).unwrap_err();
        assert_eq!(err.code, ErrorCode::TrustUnconfigured);
        let policy = TrustPolicy::new(Vec::new(), Some("ABCDE12345".to_owned()));
        assert_eq!(required_team_id(&policy).unwrap(), "ABCDE12345");
    }

    /// Runs the real `renamex_np` on the test volume. On APFS the names are
    /// exchanged; where the volume cannot, the contract is that nothing moved.
    /// Either way no scratch directory is left behind.
    #[test]
    fn the_real_exchange_either_swaps_or_changes_nothing() {
        let dir = TempDir::new("macos-real-swap");
        let (first, second) = (dir.path().join("first"), dir.path().join("second"));
        fs::create_dir(&first).unwrap();
        fs::create_dir(&second).unwrap();
        fs::write(first.join("which"), b"first").unwrap();
        fs::write(second.join("which"), b"second").unwrap();
        let supported = MacHost.supports_swap(dir.path(), dir.path()).unwrap();
        let leftovers: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .filter(|entry| entry.file_name().to_string_lossy().starts_with(".hane-swap-probe"))
            .collect();
        assert!(leftovers.is_empty(), "the probe cleaned up after itself");
        let swapped = MacHost.swap(&first, &second).is_ok();
        assert_eq!(swapped, supported, "the probe predicts the real exchange");
        let (a, b) = (fs::read(first.join("which")).unwrap(), fs::read(second.join("which")).unwrap());
        if swapped {
            assert_eq!((a.as_slice(), b.as_slice()), (&b"second"[..], &b"first"[..]));
        } else {
            assert_eq!((a.as_slice(), b.as_slice()), (&b"first"[..], &b"second"[..]));
        }
        // A missing side is an error and moves nothing.
        assert!(MacHost.swap(&first, &dir.path().join("absent")).is_err());
    }

    fn facts(dev: &str) -> VolumeFacts {
        VolumeFacts {
            id: dev.to_owned(),
            apfs: true,
            local: true,
            read_only: false,
        }
    }

    fn id_for(dev: &str, uuid: Result<String>) -> Result<InstallId> {
        derive_install_id(&facts(dev), uuid, "/Applications/Hane.app", "alice")
    }

    #[test]
    fn the_install_id_follows_the_volume_uuid_not_st_dev() {
        let uuid = || Ok("0123456789abcdef0123456789abcdef".to_owned());
        let before = id_for("1000004", uuid()).unwrap();
        let after_remount = id_for("1000009", uuid()).unwrap();
        assert_eq!(before, after_remount);
        let other = id_for("1000004", Ok("fedcba9876543210fedcba9876543210".to_owned())).unwrap();
        assert_ne!(before, other);
    }

    #[test]
    fn a_missing_volume_uuid_fails_closed() {
        let failure = Err(UpdateError::new(ErrorCode::Io, "no uuid"));
        let err = id_for("1000004", failure).unwrap_err();
        assert_eq!(err.code, ErrorCode::Io);
        assert!(uuid_to_hex(&[0u8; 16]).is_none());
        assert!(uuid_to_hex(&[1u8; 15]).is_none());
        assert_eq!(uuid_to_hex(&[0xab; 16]).unwrap(), "ab".repeat(16));
    }

    #[test]
    fn a_bare_binary_is_not_an_updatable_placement() {
        assert!(locate(Path::new("/bin/ls")).is_err());
    }
}
