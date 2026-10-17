//! macOS placement checks: a Developer ID signed `Hane.app` on a local,
//! writable APFS volume, running from its normal location.
//!
//! The volume file-system query is the only `unsafe` here. Signature checking
//! is delegated to the OS `codesign` tool with fixed arguments (no shell); the
//! catalog signature and Apple's signature are separate checks and neither
//! replaces the other. `renamex_np(RENAME_SWAP)` capability is checked by the
//! bundle-swap increment, not here.

#![allow(unsafe_code, reason = "one read-only statfs query through libc")]

use std::ffi::CString;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::process::{Command, Stdio};

use super::{HELPER_FILE_NAME, HelperSource, Placement, owner_name, unsupported};
use crate::error::{ErrorCode, Result, UpdateError};
use crate::fsutil::{hash_regular_file, read_limited};
use crate::identity::{FileIdentity, InstallFacts, InstallId};
use crate::model::{MACOS_BUNDLE_ID, Target};

const CODESIGN: &str = "/usr/bin/codesign";
const MAIN_EXECUTABLE: &str = "hane";
const MAX_PLIST_BYTES: u64 = 256 << 10;

struct Volume {
    id: String,
    apfs: bool,
    local: bool,
    read_only: bool,
}

fn query_volume(path: &Path) -> Result<Volume> {
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
        return Err(unsupported("the install volume could not be queried"));
    }
    let name_bytes: Vec<u8> = stat
        .f_fstypename
        .iter()
        .take_while(|c| **c != 0)
        .map(|c| *c as u8)
        .collect();
    let flags = i64::from(stat.f_flags);
    Ok(Volume {
        id: format!("{:x}", fs::metadata(path)?.dev()),
        apfs: name_bytes == b"apfs",
        local: flags & i64::from(libc::MNT_LOCAL) != 0,
        read_only: flags & i64::from(libc::MNT_RDONLY) != 0,
    })
}

/// Reads `CFBundleIdentifier` from an XML Info.plist; anything else fails closed.
fn plist_string(plist: &str, key: &str) -> Option<String> {
    let marker = format!("<key>{key}</key>");
    let rest = &plist[plist.find(&marker)? + marker.len()..];
    let start = rest.find("<string>")? + "<string>".len();
    let end = rest[start..].find("</string>")? + start;
    Some(rest[start..end].trim().to_owned())
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
    let plist = read_limited(&contents.join("Info.plist"), MAX_PLIST_BYTES)?
        .ok_or_else(|| unsupported("Info.plist is missing"))?;
    let plist = String::from_utf8(plist).map_err(|_| unsupported("Info.plist is not XML"))?;
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
        owner: &owner_name(),
    })?;
    Ok(Placement {
        target: Target::MacosArm64,
        install_root: bundle.to_path_buf(),
        coordination_parent,
        install_id,
    })
}

fn valid_team_id(team_id: &str) -> bool {
    team_id.len() == 10 && team_id.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
}

/// Asks the OS to verify the bundle against the Developer ID requirement (and
/// the Team ID when one is known).
pub fn verify_developer_id(bundle: &Path, team_id: Option<&str>) -> Result<()> {
    let mut requirement = String::from(
        "anchor apple generic and certificate 1[field.1.2.840.113635.100.6.2.6] exists \
         and certificate leaf[field.1.2.840.113635.100.6.1.13] exists",
    );
    if let Some(team_id) = team_id {
        if !valid_team_id(team_id) {
            return Err(UpdateError::new(ErrorCode::IdentityMismatch, "malformed Team ID"));
        }
        requirement.push_str(&format!(" and certificate leaf[subject.OU] = \"{team_id}\""));
    }
    let status = Command::new(CODESIGN)
        .arg("--verify")
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

/// The helper inside the sealed bundle, accepted only if the bundle verifies.
pub fn installed_helper(placement: &Placement) -> Result<HelperSource> {
    verify_developer_id(&placement.install_root, None)?;
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

#[cfg(test)]
mod tests {
    use super::*;

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
        assert!(verify_developer_id(Path::new("/nonexistent"), Some("bad")).is_err());
    }

    #[test]
    fn a_bare_binary_is_not_an_updatable_placement() {
        assert!(locate(Path::new("/bin/ls")).is_err());
    }
}
