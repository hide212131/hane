//! Ownership and write-permission checks for the coordination tree.
//!
//! Everything under `.hane-update-<InstallId>/` (root, `private/`, the
//! transaction and helper directories, lock files, the journal and the helper
//! copy) must belong to the current OS principal and must not be writable by
//! anyone else. A name that already exists is never adopted on trust: a
//! predictable path could have been created first by another user. Any
//! failure to read or set ownership/permissions is a refusal (fail closed),
//! never "assume it is fine".
//!
//! POSIX uses the effective UID and the mode bits. Windows uses the process
//! token's SID and the object's DACL. `USERNAME`/`USER` are never consulted.

#![allow(unsafe_code, reason = "owner and ACL queries through libc / windows-sys")]

use std::path::Path;

use crate::error::{ErrorCode, Result, UpdateError};

fn refuse(detail: impl Into<String>) -> UpdateError {
    UpdateError::new(ErrorCode::UnsupportedInstall, detail)
}

/// Restricts a path this process has just created to the current principal.
/// `mode` is the POSIX mode; Windows installs an owner-only protected DACL
/// (inheritable for directories, so files created inside stay owner-only).
pub(crate) fn secure_new(path: &Path, is_dir: bool, mode: u32) -> Result<()> {
    imp::secure_new(path, is_dir, mode)
}

/// Requires a real (non-link) directory or file that is owned by the current
/// principal and not writable by any other principal.
pub(crate) fn verify(path: &Path, is_dir: bool) -> Result<()> {
    imp::verify(path, is_dir)
}

/// A stable text form of the current OS principal (effective UID, or the
/// process token's user SID). It cannot be changed by the launching
/// environment and is the same principal `verify` compares ownership with.
#[cfg_attr(
    not(any(target_os = "windows", target_os = "macos", test)),
    allow(dead_code, reason = "only supported placements derive an InstallId owner")
)]
pub(crate) fn principal_id() -> Result<String> {
    imp::principal_id()
}

/// The owner of a real (non-link) directory in the same text form as
/// [`principal_id`]. A link, a non-directory or an unreadable owner is an
/// error that is never `UnsupportedInstall`, so startup cannot read it as an
/// unmanaged install.
#[cfg_attr(
    not(any(target_os = "windows", target_os = "macos", test)),
    allow(dead_code, reason = "only supported placements check the root owner")
)]
pub(crate) fn directory_owner_id(path: &Path) -> Result<String> {
    imp::directory_owner_id(path)
}

/// Access to the extended (non-POSIX) ACL of a path, without following links.
/// Any `Err` means the ACL could not be determined or changed.
#[cfg_attr(
    not(any(target_os = "macos", test)),
    allow(dead_code, reason = "only macOS inherits extended ACLs from the parent")
)]
trait ExtendedAcl {
    /// Whether the path carries any extended ACL entry list.
    fn present(&self, path: &Path) -> Result<bool>;
    /// Removes the extended ACL from the path.
    fn clear(&self, path: &Path) -> Result<()>;
}

/// Removes an inherited extended ACL from a path just created and proves it
/// is gone. Any failure, or a remaining ACL, is a refusal.
#[cfg_attr(
    not(any(target_os = "macos", test)),
    allow(dead_code, reason = "only macOS inherits extended ACLs from the parent")
)]
fn strip_extended_acl(ops: &dyn ExtendedAcl, path: &Path) -> Result<()> {
    if ops.present(path)? {
        ops.clear(path)?;
        if ops.present(path)? {
            return Err(refuse(format!(
                "an extended ACL on {} could not be removed",
                path.display()
            )));
        }
    }
    Ok(())
}

/// Refuses an existing entry that carries any extended ACL.
#[cfg_attr(
    not(any(target_os = "macos", test)),
    allow(dead_code, reason = "only macOS inherits extended ACLs from the parent")
)]
fn require_no_extended_acl(ops: &dyn ExtendedAcl, path: &Path) -> Result<()> {
    if ops.present(path)? {
        return Err(refuse(format!(
            "{} has an extended ACL that grants access to other principals",
            path.display()
        )));
    }
    Ok(())
}

#[cfg(target_os = "macos")]
mod macos_acl {
    use std::ffi::{c_int, c_void};
    use std::fs::{File, OpenOptions};
    use std::os::unix::fs::OpenOptionsExt;
    use std::os::unix::io::AsRawFd;
    use std::path::Path;

    use super::{ExtendedAcl, refuse};
    use crate::error::Result;

    const ACL_TYPE_EXTENDED: c_int = 0x0000_0100;
    /// `FILESEC_ACL` of `filesec_property_t`.
    const FILESEC_ACL: c_int = 5;
    /// `_FILESEC_REMOVE_ACL`: the sentinel pointer `((acl_t)1)` that asks the
    /// extended chmod call to delete the ACL.
    const FILESEC_REMOVE_ACL: usize = 1;

    unsafe extern "C" {
        fn acl_get_fd_np(fd: c_int, acl_type: c_int) -> *mut c_void;
        fn acl_free(obj: *mut c_void) -> c_int;
        fn filesec_init() -> *mut c_void;
        fn filesec_set_property(fsec: *mut c_void, property: c_int, value: *const c_void) -> c_int;
        fn filesec_free(fsec: *mut c_void);
        fn fchmodx_np(fd: c_int, fsec: *mut c_void) -> c_int;
    }

    /// A descriptor opened without following links. The ACL query, removal and
    /// read-back all use this one descriptor, so the path is never resolved
    /// again.
    pub struct Descriptor(File);

    impl Descriptor {
        pub fn open(path: &Path) -> Result<Self> {
            let file = OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
                .open(path)
                .map_err(|err| {
                    refuse(format!(
                        "{} could not be opened without following links: {err}",
                        path.display()
                    ))
                })?;
            Ok(Self(file))
        }

        pub fn file(&self) -> &File {
            &self.0
        }
    }

    /// Frees a `filesec_t` on every exit path.
    struct FileSec(*mut c_void);

    impl Drop for FileSec {
        fn drop(&mut self) {
            // SAFETY: the pointer came from `filesec_init` and is freed once.
            unsafe { filesec_free(self.0) };
        }
    }

    impl ExtendedAcl for Descriptor {
        fn present(&self, path: &Path) -> Result<bool> {
            // SAFETY: the descriptor is open; the returned ACL is freed below.
            let acl = unsafe { acl_get_fd_np(self.0.as_raw_fd(), ACL_TYPE_EXTENDED) };
            if acl.is_null() {
                // ENOENT from this call means "no extended ACL"; anything else
                // (unsupported volume, permission, ...) is undeterminable.
                let err = std::io::Error::last_os_error();
                if err.raw_os_error() == Some(libc::ENOENT) {
                    return Ok(false);
                }
                return Err(refuse(format!(
                    "the extended ACL of {} could not be read: {err}",
                    path.display()
                )));
            }
            // SAFETY: `acl` came from `acl_get_fd_np` and is freed once.
            unsafe { acl_free(acl) };
            Ok(true)
        }

        fn clear(&self, path: &Path) -> Result<()> {
            // SAFETY: `filesec_init` has no preconditions; null is checked.
            let raw = unsafe { filesec_init() };
            if raw.is_null() {
                return Err(refuse(format!(
                    "the extended ACL of {} could not be removed: {}",
                    path.display(),
                    std::io::Error::last_os_error()
                )));
            }
            let fsec = FileSec(raw);
            // SAFETY: `fsec.0` is a live filesec; for `FILESEC_ACL` the value
            // is the ACL pointer itself, here the remove sentinel.
            let set = unsafe {
                filesec_set_property(fsec.0, FILESEC_ACL, FILESEC_REMOVE_ACL as *const c_void)
            };
            if set != 0 {
                return Err(refuse(format!(
                    "the extended ACL of {} could not be removed: {}",
                    path.display(),
                    std::io::Error::last_os_error()
                )));
            }
            // SAFETY: the descriptor is open and `fsec.0` is a live filesec. The
            // mode is left unchanged because no mode property is set.
            let rc = unsafe { fchmodx_np(self.0.as_raw_fd(), fsec.0) };
            if rc != 0 {
                return Err(refuse(format!(
                    "the extended ACL of {} could not be removed: {}",
                    path.display(),
                    std::io::Error::last_os_error()
                )));
            }
            Ok(())
        }
    }
}

#[cfg(unix)]
mod imp {
    use std::fs;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    use std::path::Path;

    use super::refuse;
    use crate::error::Result;

    /// Sets the mode and removes any extended ACL through one no-follow
    /// descriptor. The ACL is removed after the mode and read back.
    #[cfg(target_os = "macos")]
    pub(super) fn secure_entry(path: &Path, mode: u32) -> Result<()> {
        let descriptor = super::macos_acl::Descriptor::open(path)?;
        descriptor
            .file()
            .set_permissions(fs::Permissions::from_mode(mode))?;
        super::strip_extended_acl(&descriptor, path)
    }

    #[cfg(not(target_os = "macos"))]
    pub(super) fn secure_entry(path: &Path, mode: u32) -> Result<()> {
        fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
        Ok(())
    }

    /// Refuses an ACL on the entry `meta` describes; the no-follow descriptor
    /// must be that same file.
    #[cfg(target_os = "macos")]
    fn require_no_acl(path: &Path, meta: &fs::Metadata) -> Result<()> {
        let descriptor = super::macos_acl::Descriptor::open(path)?;
        let opened = descriptor.file().metadata()?;
        if opened.dev() != meta.dev() || opened.ino() != meta.ino() {
            return Err(refuse(format!("{} changed while being checked", path.display())));
        }
        super::require_no_extended_acl(&descriptor, path)
    }

    #[cfg(not(target_os = "macos"))]
    fn require_no_acl(_path: &Path, _meta: &fs::Metadata) -> Result<()> {
        Ok(())
    }

    pub fn principal_id() -> Result<String> {
        // SAFETY: `geteuid` has no preconditions and cannot fail.
        let euid = unsafe { libc::geteuid() };
        Ok(format!("uid:{euid}"))
    }

    pub fn directory_owner_id(path: &Path) -> Result<String> {
        let meta = fs::symlink_metadata(path)?;
        if meta.file_type().is_symlink() || !meta.is_dir() {
            return Err(crate::error::UpdateError::new(
                crate::error::ErrorCode::IdentityMismatch,
                format!("{} is not a plain directory", path.display()),
            ));
        }
        Ok(format!("uid:{}", meta.uid()))
    }

    pub fn secure_new(path: &Path, _is_dir: bool, mode: u32) -> Result<()> {
        // On macOS the ACL is removed and read back after the mode is set,
        // so nothing remains when this returns.
        secure_entry(path, mode)
    }

    pub fn verify(path: &Path, is_dir: bool) -> Result<()> {
        let meta = fs::symlink_metadata(path)?;
        let kind_ok = if is_dir { meta.is_dir() } else { meta.is_file() };
        if meta.file_type().is_symlink() || !kind_ok {
            return Err(refuse(format!("{} is not a plain entry", path.display())));
        }
        // SAFETY: `geteuid` has no preconditions and cannot fail.
        let euid = unsafe { libc::geteuid() };
        if meta.uid() != euid {
            return Err(refuse(format!(
                "{} is not owned by the current user",
                path.display()
            )));
        }
        if meta.mode() & 0o022 != 0 {
            return Err(refuse(format!(
                "{} is writable by group or others",
                path.display()
            )));
        }
        require_no_acl(path, &meta)?;
        Ok(())
    }
}

#[cfg(windows)]
mod imp {
    use std::ffi::c_void;
    use std::fs;
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::fs::MetadataExt;
    use std::path::Path;

    use windows_sys::Win32::Foundation::{CloseHandle, LocalFree};
    use windows_sys::Win32::Security::Authorization::{
        GetNamedSecurityInfoW, SE_FILE_OBJECT, SetNamedSecurityInfoW,
    };
    use windows_sys::Win32::Security::{
        ACCESS_ALLOWED_ACE, ACE_HEADER, ACL, AddAccessAllowedAceEx, DACL_SECURITY_INFORMATION,
        EqualSid, GetAce, GetLengthSid, GetTokenInformation, InitializeAcl, IsValidSid,
        IsWellKnownSid, OWNER_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION,
        TOKEN_QUERY,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    use super::refuse;
    use crate::error::{Result, UpdateError};

    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    const TOKEN_USER_CLASS: i32 = 1;
    const TOKEN_OWNER_CLASS: i32 = 4;
    const WIN_LOCAL_SYSTEM_SID: i32 = 22;
    const WIN_BUILTIN_ADMINISTRATORS_SID: i32 = 26;
    const ACL_REVISION: u32 = 2;
    const FILE_ALL_ACCESS: u32 = 0x001F_01FF;
    const INHERIT_CONTAINER_AND_OBJECT: u32 = 0x3;
    /// FILE_WRITE_DATA, FILE_APPEND_DATA, FILE_WRITE_EA, FILE_DELETE_CHILD,
    /// FILE_WRITE_ATTRIBUTES, DELETE, WRITE_DAC, WRITE_OWNER, GENERIC_WRITE and
    /// GENERIC_ALL.
    const WRITE_MASK: u32 =
        0x0002 | 0x0004 | 0x0010 | 0x0040 | 0x0100 | 0x0001_0000 | 0x0004_0000 | 0x0008_0000
            | 0x4000_0000
            | 0x1000_0000;
    /// Allow-type ACEs this check does not interpret (compound, object,
    /// callback): refused rather than guessed at.
    const OPAQUE_ALLOW_TYPES: [u8; 4] = [4, 5, 9, 11];

    fn failure(what: &str) -> UpdateError {
        refuse(format!("{what} could not be determined"))
    }

    fn wide(path: &Path) -> Vec<u16> {
        path.as_os_str().encode_wide().chain(Some(0)).collect()
    }

    /// A security descriptor allocated by the system.
    struct Local(*mut c_void);

    impl Drop for Local {
        fn drop(&mut self) {
            if !self.0.is_null() {
                // SAFETY: the pointer came from a security-info call that
                // documents `LocalFree` as the release function.
                unsafe { LocalFree(self.0) };
            }
        }
    }

    /// Copies a SID into DWORD-aligned storage.
    fn copy_sid(sid: *mut c_void) -> Result<Vec<u32>> {
        // SAFETY: `sid` points to a SID inside a buffer that is alive here;
        // validity is checked before the length is read.
        unsafe {
            if sid.is_null() || IsValidSid(sid) == 0 {
                return Err(failure("a security identifier"));
            }
            let len = GetLengthSid(sid) as usize;
            let mut copy = vec![0_u32; len.div_ceil(4)];
            std::ptr::copy_nonoverlapping(sid.cast::<u8>(), copy.as_mut_ptr().cast::<u8>(), len);
            Ok(copy)
        }
    }

    fn sid_ptr(sid: &[u32]) -> *mut c_void {
        sid.as_ptr() as *mut c_void
    }

    fn same_sid(a: *mut c_void, b: &[u32]) -> bool {
        // SAFETY: both pointers refer to valid SIDs held by the caller.
        unsafe { EqualSid(a, sid_ptr(b)) != 0 }
    }

    /// The process token's user SID (class 1) or default owner SID (class 4).
    /// Both token structures start with the SID pointer.
    fn token_sid(class: i32) -> Result<Vec<u32>> {
        let mut token = std::ptr::null_mut();
        // SAFETY: `token` is a live out-pointer; the pseudo handle needs no close.
        if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
            return Err(failure("the process token"));
        }
        let mut needed = 0_u32;
        // SAFETY: a null buffer of length 0 asks only for the required size.
        unsafe { GetTokenInformation(token, class, std::ptr::null_mut(), 0, &mut needed) };
        let result = if needed == 0 {
            Err(failure("the process token"))
        } else {
            let mut buffer = vec![0_u64; (needed as usize).div_ceil(8)];
            // SAFETY: the buffer is `buffer.len() * 8` bytes, 8-byte aligned.
            let ok = unsafe {
                GetTokenInformation(
                    token,
                    class,
                    buffer.as_mut_ptr().cast(),
                    (buffer.len() * 8) as u32,
                    &mut needed,
                )
            };
            if ok == 0 {
                Err(failure("the process token"))
            } else {
                // SAFETY: on success the buffer begins with a SID pointer.
                let sid = unsafe { *buffer.as_ptr().cast::<*mut c_void>() };
                copy_sid(sid)
            }
        };
        // SAFETY: `token` was opened above and is closed exactly once.
        unsafe { CloseHandle(token) };
        result
    }

    fn well_known(sid: *mut c_void, kind: i32) -> bool {
        // SAFETY: `sid` is a valid SID.
        unsafe { IsWellKnownSid(sid, kind) != 0 }
    }

    fn sid_text(sid: *mut c_void) -> String {
        // SAFETY: the caller passes a valid SID that outlives this call.
        let len = unsafe { GetLengthSid(sid) } as usize;
        let bytes = unsafe { std::slice::from_raw_parts(sid.cast::<u8>(), len) };
        format!(
            "sid:{}",
            bytes.iter().map(|b| format!("{b:02x}")).collect::<String>()
        )
    }

    pub fn principal_id() -> Result<String> {
        let sid = token_sid(TOKEN_USER_CLASS)?;
        Ok(sid_text(sid_ptr(&sid)))
    }

    pub fn directory_owner_id(path: &Path) -> Result<String> {
        let meta = fs::symlink_metadata(path)?;
        if meta.file_type().is_symlink()
            || meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
            || !meta.is_dir()
        {
            return Err(UpdateError::new(
                crate::error::ErrorCode::IdentityMismatch,
                format!("{} is not a plain directory", path.display()),
            ));
        }
        let name = wide(path);
        let mut owner: *mut c_void = std::ptr::null_mut();
        let mut descriptor: *mut c_void = std::ptr::null_mut();
        // SAFETY: `name` is NUL-terminated and every out-pointer is a live local.
        let rc = unsafe {
            GetNamedSecurityInfoW(
                name.as_ptr(),
                SE_FILE_OBJECT,
                OWNER_SECURITY_INFORMATION,
                &mut owner,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut descriptor,
            )
        };
        let _release = Local(descriptor);
        // SAFETY: a non-null owner points into the descriptor kept alive above.
        if rc != 0 || owner.is_null() || unsafe { IsValidSid(owner) } == 0 {
            return Err(UpdateError::new(
                crate::error::ErrorCode::Io,
                format!("the owner of {} could not be determined", path.display()),
            ));
        }
        Ok(sid_text(owner))
    }

    pub fn secure_new(path: &Path, is_dir: bool, _mode: u32) -> Result<()> {
        let user = token_sid(TOKEN_USER_CLASS)?;
        let mut acl = vec![0_u32; 128];
        let flags = if is_dir { INHERIT_CONTAINER_AND_OBJECT } else { 0 };
        // SAFETY: `acl` is DWORD-aligned and its byte length is passed; the
        // SID is a valid copy that outlives the call.
        let built = unsafe {
            InitializeAcl(acl.as_mut_ptr().cast(), (acl.len() * 4) as u32, ACL_REVISION) != 0
                && AddAccessAllowedAceEx(
                    acl.as_mut_ptr().cast(),
                    ACL_REVISION,
                    flags,
                    FILE_ALL_ACCESS,
                    sid_ptr(&user),
                ) != 0
        };
        if !built {
            return Err(failure("an owner-only access list"));
        }
        let name = wide(path);
        // SAFETY: `name` is NUL-terminated; `acl` is an initialized ACL. The
        // protected flag stops parent ACEs from being inherited.
        let rc = unsafe {
            SetNamedSecurityInfoW(
                name.as_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                acl.as_ptr().cast::<ACL>(),
                std::ptr::null(),
            )
        };
        if rc != 0 {
            return Err(refuse(format!(
                "the access list of {} could not be set",
                path.display()
            )));
        }
        Ok(())
    }

    pub fn verify(path: &Path, is_dir: bool) -> Result<()> {
        let meta = fs::symlink_metadata(path)?;
        let kind_ok = if is_dir { meta.is_dir() } else { meta.is_file() };
        if meta.file_type().is_symlink()
            || meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
            || !kind_ok
        {
            return Err(refuse(format!("{} is not a plain entry", path.display())));
        }
        let user = token_sid(TOKEN_USER_CLASS)?;
        let default_owner = token_sid(TOKEN_OWNER_CLASS)?;
        let name = wide(path);
        let mut owner: *mut c_void = std::ptr::null_mut();
        let mut dacl: *mut ACL = std::ptr::null_mut();
        let mut descriptor: *mut c_void = std::ptr::null_mut();
        // SAFETY: `name` is NUL-terminated and every out-pointer is a live local.
        let rc = unsafe {
            GetNamedSecurityInfoW(
                name.as_ptr(),
                SE_FILE_OBJECT,
                OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
                &mut owner,
                std::ptr::null_mut(),
                &mut dacl,
                std::ptr::null_mut(),
                &mut descriptor,
            )
        };
        let _release = Local(descriptor);
        if rc != 0 {
            return Err(failure(&format!("the security of {}", path.display())));
        }
        // A missing DACL grants everyone full access.
        if owner.is_null() || dacl.is_null() {
            return Err(refuse(format!("{} has no owner-only access list", path.display())));
        }
        if !same_sid(owner, &user) && !same_sid(owner, &default_owner) {
            return Err(refuse(format!(
                "{} is not owned by the current user",
                path.display()
            )));
        }
        let trusted = |sid: *mut c_void| {
            same_sid(sid, &user)
                || same_sid(sid, &default_owner)
                || well_known(sid, WIN_LOCAL_SYSTEM_SID)
                || well_known(sid, WIN_BUILTIN_ADMINISTRATORS_SID)
        };
        // SAFETY: `dacl` points into the descriptor kept alive by `_release`;
        // each ACE pointer comes from `GetAce` for an index below the count.
        unsafe {
            for index in 0..u32::from((*dacl).AceCount) {
                let mut ace: *mut c_void = std::ptr::null_mut();
                if GetAce(dacl, index, &mut ace) == 0 || ace.is_null() {
                    return Err(failure("an access control entry"));
                }
                let kind = (*ace.cast::<ACE_HEADER>()).AceType;
                if OPAQUE_ALLOW_TYPES.contains(&kind) {
                    return Err(refuse(format!(
                        "{} has an access entry that cannot be checked",
                        path.display()
                    )));
                }
                if kind != 0 {
                    continue;
                }
                let allowed = &*ace.cast::<ACCESS_ALLOWED_ACE>();
                let sid = std::ptr::addr_of!(allowed.SidStart) as *mut c_void;
                if allowed.Mask & WRITE_MASK != 0 && !trusted(sid) {
                    return Err(refuse(format!(
                        "{} is writable by another principal",
                        path.display()
                    )));
                }
            }
        }
        Ok(())
    }
}

#[cfg(not(any(unix, windows)))]
mod imp {
    use std::path::Path;

    use super::refuse;
    use crate::error::Result;

    pub fn principal_id() -> Result<String> {
        Err(refuse("the OS principal is unavailable on this OS"))
    }

    pub fn directory_owner_id(_path: &Path) -> Result<String> {
        Err(refuse("ownership cannot be checked on this OS"))
    }

    pub fn secure_new(_path: &Path, _is_dir: bool, _mode: u32) -> Result<()> {
        Err(refuse("ownership cannot be checked on this OS"))
    }

    pub fn verify(_path: &Path, _is_dir: bool) -> Result<()> {
        Err(refuse("ownership cannot be checked on this OS"))
    }
}

// The fake below exercises the fail-closed logic on every OS. The macOS
// module at the end uses real ACLs (`chmod +a`, `fchmodx_np`).
#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::path::Path;

    use super::{ExtendedAcl, require_no_extended_acl, strip_extended_acl};
    use crate::error::{Result, UpdateError};

    struct Fake {
        present: Cell<bool>,
        sticky: bool,
        fail_present: bool,
        fail_clear: bool,
    }

    impl Fake {
        fn new(present: bool) -> Self {
            Self {
                present: Cell::new(present),
                sticky: false,
                fail_present: false,
                fail_clear: false,
            }
        }
    }

    impl ExtendedAcl for Fake {
        fn present(&self, _path: &Path) -> Result<bool> {
            if self.fail_present {
                return Err(super::refuse("query failed"));
            }
            Ok(self.present.get())
        }

        fn clear(&self, _path: &Path) -> Result<()> {
            if self.fail_clear {
                return Err(super::refuse("clear failed"));
            }
            if !self.sticky {
                self.present.set(false);
            }
            Ok(())
        }
    }

    fn is_refusal<T>(result: Result<T>) -> bool {
        matches!(result, Err(UpdateError { .. }))
    }

    #[test]
    fn inherited_acl_is_removed_from_new_entry() {
        let fake = Fake::new(true);
        strip_extended_acl(&fake, Path::new("x")).unwrap();
        assert!(!fake.present.get());
    }

    #[test]
    fn new_entry_without_acl_is_accepted() {
        strip_extended_acl(&Fake::new(false), Path::new("x")).unwrap();
    }

    #[test]
    fn new_entry_fails_closed_when_acl_remains() {
        let mut fake = Fake::new(true);
        fake.sticky = true;
        assert!(is_refusal(strip_extended_acl(&fake, Path::new("x"))));
    }

    #[test]
    fn new_entry_fails_closed_on_acl_errors() {
        let mut query = Fake::new(true);
        query.fail_present = true;
        assert!(is_refusal(strip_extended_acl(&query, Path::new("x"))));
        let mut clear = Fake::new(true);
        clear.fail_clear = true;
        assert!(is_refusal(strip_extended_acl(&clear, Path::new("x"))));
    }

    #[test]
    fn existing_entry_with_acl_is_refused() {
        assert!(is_refusal(require_no_extended_acl(&Fake::new(true), Path::new("x"))));
        require_no_extended_acl(&Fake::new(false), Path::new("x")).unwrap();
    }

    #[test]
    fn existing_entry_fails_closed_when_acl_is_undeterminable() {
        let mut fake = Fake::new(false);
        fake.fail_present = true;
        assert!(is_refusal(require_no_extended_acl(&fake, Path::new("x"))));
    }

    #[cfg(target_os = "macos")]
    mod real_acl {
        use std::path::{Path, PathBuf};
        use std::process::Command;

        use super::super::{ExtendedAcl, imp, macos_acl::Descriptor};

        fn present(path: &Path) -> bool {
            Descriptor::open(path).unwrap().present(path).unwrap()
        }

        fn scratch(name: &str) -> PathBuf {
            let dir = std::env::temp_dir().join(format!(
                "hane-acl-{name}-{}-{:?}",
                std::process::id(),
                std::time::SystemTime::now()
            ));
            std::fs::create_dir_all(&dir).unwrap();
            dir
        }

        fn add_acl(path: &Path) {
            let status = Command::new("/bin/chmod")
                .arg("+a")
                .arg("everyone allow write,delete")
                .arg(path)
                .status()
                .unwrap();
            assert!(status.success(), "chmod +a failed");
        }

        #[test]
        fn real_acl_is_removed_from_new_entry() {
            let dir = scratch("new");
            let file = dir.join("entry");
            std::fs::write(&file, b"").unwrap();
            add_acl(&file);
            assert!(present(&file));
            imp::secure_new(&file, false, 0o600).unwrap();
            assert!(!present(&file));
            imp::verify(&file, false).unwrap();
            std::fs::remove_dir_all(&dir).unwrap();
        }

        #[test]
        fn real_acl_on_existing_entry_is_refused() {
            let dir = scratch("existing");
            let file = dir.join("entry");
            std::fs::write(&file, b"").unwrap();
            std::fs::set_permissions(
                &file,
                std::os::unix::fs::PermissionsExt::from_mode(0o600),
            )
            .unwrap();
            add_acl(&file);
            assert!(imp::verify(&file, false).is_err());
            std::fs::remove_dir_all(&dir).unwrap();
        }
    }
}
