//! Windows placement checks: portable x64 on a local fixed NTFS volume.
//!
//! The two volume queries are the only `unsafe` in the update crate. Network
//! shares, removable media, other file systems and non-`hane.exe` binaries
//! (for example a development `cargo` output) are not supported placements.

#![allow(unsafe_code, reason = "two read-only volume queries through windows-sys")]

use std::fs;
use std::os::windows::ffi::OsStrExt;
use std::path::{Component, Path, Prefix};

use windows_sys::Win32::Foundation::{CloseHandle, FILETIME, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Storage::FileSystem::{
    GetDriveTypeW, GetVolumeInformationW, GetVolumeNameForVolumeMountPointW, GetVolumePathNameW,
};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, GetCurrentProcessId, GetProcessTimes, OpenProcess,
    PROCESS_QUERY_LIMITED_INFORMATION,
};

use super::{HELPER_FILE_NAME, HelperSource, Placement, owner_name, unsupported};
use crate::error::{ErrorCode, Result, UpdateError};
use crate::fsutil::{hash_regular_file, read_limited};
use crate::identity::{FileIdentity, InstallFacts, InstallId, SignedInventory};
use crate::limits::{MAX_INVENTORY_BYTES, MAX_SIGNATURE_BYTES};
use crate::model::{INVENTORY_FILE_NAME, INVENTORY_SIGNATURE_FILE_NAME, Target};
use crate::trust::TrustPolicy;

const EXE_NAME: &str = "hane.exe";

/// `GetDriveTypeW` result for a local fixed disk (winbase.h `DRIVE_FIXED`).
/// Defined here because windows-sys exposes it only behind the unrelated
/// `Win32_System_WindowsProgramming` feature.
const DRIVE_FIXED: u32 = 3;

/// The volume that really holds a path, including directory mount points.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Volume {
    /// Stable `\\?\Volume{GUID}\` name of the volume; the volume identity.
    guid: String,
    serial: u32,
    file_system: String,
    fixed: bool,
}

const VOLUME_BUFFER_UNITS: usize = 1024;

fn wide_path(path: &Path) -> Vec<u16> {
    path.as_os_str().encode_wide().chain(Some(0)).collect()
}

fn from_wide(buffer: &[u16]) -> String {
    let len = buffer.iter().position(|unit| *unit == 0).unwrap_or(buffer.len());
    String::from_utf16_lossy(&buffer[..len])
}

/// Query the volume that actually contains `path` (not the drive-letter root),
/// resolving directory mount points. Any failure is an error, never a guess.
fn query_volume(path: &Path) -> Result<Volume> {
    let fail = |what: &str| UpdateError::new(ErrorCode::Io, what.to_owned());
    let path_w = wide_path(path);
    let mut mount_root = [0_u16; VOLUME_BUFFER_UNITS];
    // SAFETY: `path_w` is NUL-terminated and the out buffer length is passed.
    let ok = unsafe {
        GetVolumePathNameW(path_w.as_ptr(), mount_root.as_mut_ptr(), mount_root.len() as u32)
    };
    if ok == 0 {
        return Err(fail("the install volume root could not be resolved"));
    }
    let mut guid_buf = [0_u16; 64];
    // SAFETY: `mount_root` is NUL-terminated by the previous call and the out
    // buffer length is passed.
    let ok = unsafe {
        GetVolumeNameForVolumeMountPointW(
            mount_root.as_ptr(),
            guid_buf.as_mut_ptr(),
            guid_buf.len() as u32,
        )
    };
    if ok == 0 {
        return Err(fail("the install volume identity could not be resolved"));
    }
    let guid = from_wide(&guid_buf);
    if !guid.starts_with(r"\\?\Volume{") || !guid.ends_with('\\') {
        return Err(fail("the install volume identity is not recognised"));
    }
    query_volume_info(&guid)
}

fn query_volume_info(guid: &str) -> Result<Volume> {
    let root_w = std::ffi::OsStr::new(guid)
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<u16>>();
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
        guid: guid.to_owned(),
        serial,
        file_system: String::from_utf16_lossy(&fs_name[..len]),
        fixed: drive_type == DRIVE_FIXED,
    })
}

/// Install root and coordination/stage area must share one fixed NTFS volume,
/// otherwise atomic replacement and recovery cannot be relied on.
fn ensure_supported_volumes(install: &Volume, coordination: &Volume) -> Result<()> {
    if install.guid != coordination.guid || install.serial != coordination.serial {
        return Err(unsupported(
            "the install root and its coordination area are on different volumes",
        ));
    }
    if !install.fixed {
        return Err(unsupported("only a local fixed drive is supported"));
    }
    if !install.file_system.eq_ignore_ascii_case("NTFS") {
        return Err(unsupported("only NTFS is supported"));
    }
    Ok(())
}

/// Creation time of a process as a 64-bit FILETIME, or `None` when unreadable.
fn creation_time(process: HANDLE) -> Option<u64> {
    let mut created = FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 };
    let mut exited = created;
    let mut kernel = created;
    let mut user = created;
    // SAFETY: `process` is a live process handle with query access and every
    // out-pointer refers to a live local FILETIME.
    let ok = unsafe { GetProcessTimes(process, &mut created, &mut exited, &mut kernel, &mut user) };
    (ok != 0).then(|| (u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime))
}

/// The id of the process that really started this one, as the OS records it.
/// A parent that has exited, cannot be opened, or whose id was reused by a
/// process created *after* this one is reported as `None`, never guessed.
pub fn parent_process_id() -> Option<u32> {
    // SAFETY: no arguments; returns the calling process id.
    let own = unsafe { GetCurrentProcessId() };
    // SAFETY: a plain process snapshot; the handle is closed below.
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return None;
    }
    // SAFETY: PROCESSENTRY32W is plain data for which all-zero is valid.
    let mut entry: PROCESSENTRY32W = unsafe { std::mem::zeroed() };
    entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
    let mut parent = None;
    // SAFETY: `snapshot` is valid and `entry.dwSize` is set as documented.
    let mut more = unsafe { Process32FirstW(snapshot, &mut entry) };
    while more != 0 {
        if entry.th32ProcessID == own {
            parent = Some(entry.th32ParentProcessID);
            break;
        }
        // SAFETY: same handle and entry as above.
        more = unsafe { Process32NextW(snapshot, &mut entry) };
    }
    // SAFETY: `snapshot` is a handle this function owns.
    unsafe { CloseHandle(snapshot) };
    let parent = parent?;
    // SAFETY: the current-process pseudo handle needs no closing.
    let own_created = creation_time(unsafe { GetCurrentProcess() })?;
    // SAFETY: plain open for limited query; null means failure.
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, parent) };
    if handle.is_null() {
        return None;
    }
    let parent_created = creation_time(handle);
    // SAFETY: `handle` is owned by this function.
    unsafe { CloseHandle(handle) };
    (parent_created? <= own_created).then_some(parent)
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
    if !matches!(prefix.kind(), Prefix::Disk(_) | Prefix::VerbatimDisk(_)) {
        return Err(unsupported("network and device paths are not supported"));
    }
    let volume = query_volume(&install_dir)?;
    let coordination_volume = query_volume(&coordination_parent)?;
    ensure_supported_volumes(&volume, &coordination_volume)?;
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
    let path_text = crate::identity::install_path_text(&install_dir)?;
    let path_text = path_text.strip_prefix(r"\\?\").unwrap_or(path_text).to_owned();
    let install_id = InstallId::derive(&InstallFacts {
        os: "windows",
        volume_id: &format!("{}:{:08x}", volume.guid, volume.serial),
        install_path: &path_text,
        owner: &owner_name()?,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn volume(guid: &str, serial: u32, file_system: &str, fixed: bool) -> Volume {
        Volume {
            guid: guid.to_owned(),
            serial,
            file_system: file_system.to_owned(),
            fixed,
        }
    }

    const SYSTEM: &str = r"\\?\Volume{11111111-1111-1111-1111-111111111111}\";
    const MOUNTED: &str = r"\\?\Volume{22222222-2222-2222-2222-222222222222}\";

    #[test]
    fn same_fixed_ntfs_volume_is_supported() {
        let v = volume(SYSTEM, 1, "NTFS", true);
        assert!(ensure_supported_volumes(&v, &v.clone()).is_ok());
    }

    #[test]
    fn mount_point_volume_that_is_fixed_ntfs_is_supported() {
        // Install root and coordination parent both live on the volume mounted
        // under a directory, regardless of the drive-letter volume's type.
        let v = volume(MOUNTED, 2, "ntfs", true);
        assert!(ensure_supported_volumes(&v, &v.clone()).is_ok());
    }

    #[test]
    fn install_root_that_is_a_mount_point_splits_volumes() {
        let install = volume(MOUNTED, 2, "NTFS", true);
        let coordination = volume(SYSTEM, 1, "NTFS", true);
        assert!(ensure_supported_volumes(&install, &coordination).is_err());
    }

    #[test]
    fn same_serial_but_different_volume_is_rejected() {
        let install = volume(MOUNTED, 1, "NTFS", true);
        let coordination = volume(SYSTEM, 1, "NTFS", true);
        assert!(ensure_supported_volumes(&install, &coordination).is_err());
    }

    #[test]
    fn removable_mount_point_volume_is_rejected() {
        let v = volume(MOUNTED, 2, "NTFS", false);
        assert!(ensure_supported_volumes(&v, &v.clone()).is_err());
    }

    #[test]
    fn non_ntfs_mount_point_volume_is_rejected() {
        let v = volume(MOUNTED, 2, "exFAT", true);
        assert!(ensure_supported_volumes(&v, &v.clone()).is_err());
    }
}
