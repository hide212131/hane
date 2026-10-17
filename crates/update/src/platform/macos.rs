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
    if !volume.apfs || !volume.local {
        return Err(unsupported("only a local APFS volume is supported"));
    }
    if volume.read_only {
        return Err(unsupported("the install volume is read-only"));
    }
    let install_id = InstallId::derive(&InstallFacts {
        os: "macos",
        volume_id: &format!("apfs:{}", volume.id),
        install_path: &bundle.to_string_lossy(),
        owner: &owner_name()?,
    })?;
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

/// The real [`BundleHost`]: `statfs`, `renamex_np(RENAME_SWAP)`, `codesign`
/// and `spctl`, each with fixed arguments and no shell.
pub struct MacHost;

impl BundleHost for MacHost {
    fn volume(&self, path: &Path) -> Result<VolumeFacts> {
        query_volume(path)
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

    fn verify_developer_id(&self, bundle: &Path, team_id: &str) -> Result<()> {
        verify_developer_id(bundle, team_id)
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

    #[test]
    fn a_bare_binary_is_not_an_updatable_placement() {
        assert!(locate(Path::new("/bin/ls")).is_err());
    }
}
