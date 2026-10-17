//! Windows placement checks: portable x64 on a local fixed NTFS volume.
//!
//! The two volume queries are the only `unsafe` in the update crate. Network
//! shares, removable media, other file systems and non-`hane.exe` binaries
//! (for example a development `cargo` output) are not supported placements.

#![allow(unsafe_code, reason = "two read-only volume queries through windows-sys")]

use std::fs;
use std::os::windows::ffi::OsStrExt;
use std::path::{Component, Path, Prefix};

use windows_sys::Win32::Storage::FileSystem::{DRIVE_FIXED, GetDriveTypeW, GetVolumeInformationW};

use super::{HELPER_FILE_NAME, HelperSource, Placement, owner_name, unsupported};
use crate::error::{ErrorCode, Result, UpdateError};
use crate::fsutil::{hash_regular_file, read_limited};
use crate::identity::{FileIdentity, InstallFacts, InstallId, SignedInventory};
use crate::limits::{MAX_INVENTORY_BYTES, MAX_SIGNATURE_BYTES};
use crate::model::{INVENTORY_FILE_NAME, INVENTORY_SIGNATURE_FILE_NAME, Target};
use crate::trust::TrustPolicy;

const EXE_NAME: &str = "hane.exe";

struct Volume {
    serial: u32,
    file_system: String,
    fixed: bool,
}

fn wide(text: &str) -> Vec<u16> {
    std::ffi::OsStr::new(text)
        .encode_wide()
        .chain(Some(0))
        .collect()
}

fn query_volume(root: &str) -> Result<Volume> {
    let root_w = wide(root);
    let mut serial = 0_u32;
    let mut max_component = 0_u32;
    let mut flags = 0_u32;
    let mut fs_name = [0_u16; 64];
    // SAFETY: `root_w` is NUL-terminated; every out-pointer refers to a live
    // local of the size the API documents; the volume-name buffer is omitted.
    let ok = unsafe {
        GetVolumeInformationW(
            root_w.as_ptr(),
            std::ptr::null_mut(),
            0,
            &mut serial,
            &mut max_component,
            &mut flags,
            fs_name.as_mut_ptr(),
            fs_name.len() as u32,
        )
    };
    if ok == 0 {
        return Err(UpdateError::new(
            ErrorCode::Io,
            "the install volume could not be queried",
        ));
    }
    let len = fs_name.iter().position(|unit| *unit == 0).unwrap_or(fs_name.len());
    // SAFETY: `root_w` is a NUL-terminated wide string.
    let drive_type = unsafe { GetDriveTypeW(root_w.as_ptr()) };
    Ok(Volume {
        serial,
        file_system: String::from_utf16_lossy(&fs_name[..len]),
        fixed: drive_type == DRIVE_FIXED,
    })
}

fn is_regular_file(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|meta| meta.is_file() && !meta.file_type().is_symlink())
}

pub fn locate(exe: &Path) -> Result<Placement> {
    if Target::current()? != Target::WindowsX64 {
        return Err(unsupported("not a Windows x64 build"));
    }
    let exe = fs::canonicalize(exe)?;
    let name_ok = exe
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.eq_ignore_ascii_case(EXE_NAME));
    if !name_ok {
        return Err(unsupported("the running binary is not hane.exe"));
    }
    let install_dir = exe
        .parent()
        .ok_or_else(|| unsupported("the install has no directory"))?
        .to_path_buf();
    let coordination_parent = install_dir
        .parent()
        .ok_or_else(|| unsupported("the install has no parent for coordination"))?
        .to_path_buf();
    let Some(Component::Prefix(prefix)) = install_dir.components().next() else {
        return Err(unsupported("the install path has no drive"));
    };
    let letter = match prefix.kind() {
        Prefix::Disk(letter) | Prefix::VerbatimDisk(letter) => char::from(letter),
        _ => return Err(unsupported("network and device paths are not supported")),
    };
    let volume = query_volume(&format!("{}:\\", letter.to_ascii_uppercase()))?;
    if !volume.fixed {
        return Err(unsupported("only a local fixed drive is supported"));
    }
    if !volume.file_system.eq_ignore_ascii_case("NTFS") {
        return Err(unsupported("only NTFS is supported"));
    }
    let managed_files = [HELPER_FILE_NAME, INVENTORY_FILE_NAME, INVENTORY_SIGNATURE_FILE_NAME];
    let present = managed_files
        .iter()
        .filter(|name| is_regular_file(&install_dir.join(name)))
        .count();
    if present == 0 {
        // Nothing of the managed layout exists (for example a development build).
        return Err(unsupported("the install has no updater files"));
    }
    // Some but not all: a managed install that is incomplete or mid-update.
    // That is not an unsupported placement and must not start normal editing.
    if let Some(name) = managed_files
        .iter()
        .find(|name| !is_regular_file(&install_dir.join(name)))
    {
        return Err(UpdateError::new(
            ErrorCode::InstallModified,
            format!("{name} is missing from a managed install"),
        ));
    }
    let path_text = install_dir.to_string_lossy().into_owned();
    let path_text = path_text.strip_prefix(r"\\?\").unwrap_or(&path_text).to_owned();
    let install_id = InstallId::derive(&InstallFacts {
        os: "windows",
        volume_id: &format!("{}:{:08x}", letter.to_ascii_uppercase(), volume.serial),
        install_path: &path_text,
        owner: &owner_name(),
    })?;
    Ok(Placement {
        target: Target::WindowsX64,
        install_root: install_dir,
        coordination_parent,
        install_id,
    })
}

/// The helper must be listed by the install's own signed inventory, and the
/// file on disk must match that entry.
pub fn installed_helper(placement: &Placement, policy: &TrustPolicy) -> Result<HelperSource> {
    let missing = |what: &str| UpdateError::new(ErrorCode::NotFound, format!("{what} is missing"));
    let root = &placement.install_root;
    let raw = read_limited(&root.join(INVENTORY_FILE_NAME), MAX_INVENTORY_BYTES)?
        .ok_or_else(|| missing("the install inventory"))?;
    let signature = read_limited(&root.join(INVENTORY_SIGNATURE_FILE_NAME), MAX_SIGNATURE_BYTES)?
        .ok_or_else(|| missing("the inventory signature"))?;
    let signed = SignedInventory::verify(&raw, &signature, policy)?;
    let entry = signed
        .inventory()
        .files
        .iter()
        .find(|file| file.path == HELPER_FILE_NAME)
        .ok_or_else(|| {
            UpdateError::new(ErrorCode::IdentityMismatch, "the inventory does not list the helper")
        })?;
    let identity = FileIdentity {
        size: entry.size,
        sha256: entry.sha256.clone(),
    };
    let path = root.join(HELPER_FILE_NAME);
    let on_disk = hash_regular_file(&path)?.map(|(size, sha256)| FileIdentity { size, sha256 });
    if on_disk.as_ref() != Some(&identity) {
        return Err(UpdateError::new(
            ErrorCode::InstallModified,
            "the installed helper differs from the signed inventory",
        ));
    }
    Ok(HelperSource { path, identity })
}
