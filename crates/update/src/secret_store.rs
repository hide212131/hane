//! The OS-backed [`TransientSecretStore`] for the proxy userinfo that crosses an
//! update restart: the macOS Keychain or the Windows Credential Manager, under
//! the `hane.network.proxy` service reserved for proxy credentials.
//!
//! An entry is addressed by the opaque, transaction-bound reference recorded in
//! `child-env.json`; the secret itself exists only in the OS store. There is no
//! file or plaintext fallback: where no native backend exists every `put` and
//! `take` fails, so the handoff aborts instead of continuing without the
//! credential. Errors carry fixed text only, never the secret or an OS message.
//!
//! On macOS the app creates the entry and the separate update helper takes and
//! deletes it, so the entry is created with an access list naming only the app
//! and the bundled helper (`keychain_acl`); if that list cannot be built or
//! applied, `put` fails and the handoff aborts.

#[cfg(any(target_os = "macos", target_os = "windows"))]
use hane_network::PROXY_CREDENTIAL_SERVICE;

use crate::error::{ErrorCode, Result, UpdateError};
use crate::state_root::TransientSecretStore;

/// A backend failure (no native store, or the OS store rejected the
/// operation). Deliberately without any detail text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BackendError;

/// The raw OS operations, so the one-time and error behaviour is testable
/// without touching a real Keychain / Credential Manager.
pub trait SecretBackend {
    fn set(&self, account: &str, secret: &str) -> std::result::Result<(), BackendError>;
    fn get(&self, account: &str) -> std::result::Result<Option<String>, BackendError>;
    /// Removing an absent entry is not an error.
    fn delete(&self, account: &str) -> std::result::Result<(), BackendError>;
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
pub struct KeyringBackend;

#[cfg(any(target_os = "macos", target_os = "windows"))]
impl KeyringBackend {
    fn entry(account: &str) -> std::result::Result<keyring::Entry, BackendError> {
        keyring::Entry::new(PROXY_CREDENTIAL_SERVICE, account).map_err(|_| BackendError)
    }
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
impl SecretBackend for KeyringBackend {
    fn set(&self, account: &str, secret: &str) -> std::result::Result<(), BackendError> {
        // macOS: the entry is created with an access list that names only this
        // app and its update helper, so the helper can read and delete it later
        // without a prompt (see `keychain_acl`).
        #[cfg(target_os = "macos")]
        {
            keychain_acl::create(PROXY_CREDENTIAL_SERVICE, account, secret, || {
                // A stale entry of the same reference is replaced.
                let _ = Self::entry(account)?.delete_password();
                Ok(())
            })
        }
        #[cfg(not(target_os = "macos"))]
        {
            Self::entry(account)?
                .set_password(secret)
                .map_err(|_| BackendError)
        }
    }

    fn get(&self, account: &str) -> std::result::Result<Option<String>, BackendError> {
        match Self::entry(account)?.get_password() {
            Ok(secret) => Ok(Some(secret)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(_) => Err(BackendError),
        }
    }

    fn delete(&self, account: &str) -> std::result::Result<(), BackendError> {
        match Self::entry(account)?.delete_password() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(_) => Err(BackendError),
        }
    }
}

/// Where the update helper of the bundle that contains `exe` lives:
/// `Hane.app/Contents/MacOS/hane` -> `Hane.app/Contents/Helpers/hane-updater`.
/// `None` when `exe` is not inside such a bundle layout.
#[cfg(any(target_os = "macos", test))]
fn bundled_helper_path(exe: &std::path::Path) -> Option<std::path::PathBuf> {
    let macos_dir = exe.parent()?;
    let contents = macos_dir.parent()?;
    if macos_dir.file_name()? != "MacOS" || contents.file_name()? != "Contents" {
        return None;
    }
    Some(contents.join("Helpers").join("hane-updater"))
}

/// The applications a Keychain entry trusts: the running app executable and its
/// bundle's `hane-updater`, nothing else. `None` outside the bundle layout.
#[cfg(any(target_os = "macos", test))]
fn trusted_application_paths(exe: &std::path::Path) -> Option<[std::path::PathBuf; 2]> {
    let helper = bundled_helper_path(exe)?;
    Some([exe.to_path_buf(), helper])
}

/// Keychain items created by the app carry, by default, an access list that
/// trusts only the creating executable; the update helper is another executable
/// and would be refused (or prompted) when it takes and deletes the item. The
/// item is therefore created with an explicit access list of exactly two
/// applications: this executable and the bundle's `hane-updater`.
///
/// Trust is by code requirement, not by path: for the ad-hoc signatures of
/// `scripts/bundle-macos.sh` that is the code directory hash. The helper runs
/// from a byte-identical copy of the installed helper (`stage_helper_copy`), so
/// it satisfies the same requirement; any other executable does not.
#[cfg(target_os = "macos")]
#[allow(unsafe_code, reason = "legacy Keychain access-list calls through Security.framework")]
mod keychain_acl {
    use std::ffi::{CString, c_char, c_void};
    use std::os::unix::ffi::OsStrExt;
    use std::ptr;

    use core_foundation_sys::array::{CFArrayCreate, kCFTypeArrayCallBacks};
    use core_foundation_sys::base::{CFIndex, CFRelease, kCFAllocatorDefault};
    use core_foundation_sys::data::CFDataCreate;
    use core_foundation_sys::dictionary::{
        CFDictionaryCreate, kCFTypeDictionaryKeyCallBacks, kCFTypeDictionaryValueCallBacks,
    };
    use core_foundation_sys::string::{
        CFStringCreateWithBytes, CFStringCreateWithCString, kCFStringEncodingUTF8,
    };

    use super::{BackendError, trusted_application_paths};

    type OsStatus = i32;
    type Ref = *const c_void;

    /// `errSecDuplicateItem`: an entry of the same service / account exists.
    const DUPLICATE_ITEM: OsStatus = -25299;

    #[link(name = "Security", kind = "framework")]
    unsafe extern "C" {
        fn SecTrustedApplicationCreateFromPath(path: *const c_char, app: *mut Ref) -> OsStatus;
        fn SecAccessCreate(descriptor: Ref, trusted_list: Ref, access: *mut Ref) -> OsStatus;
        fn SecItemAdd(attributes: Ref, result: *mut Ref) -> OsStatus;

        #[link_name = "kSecClass"]
        static SEC_CLASS: Ref;
        #[link_name = "kSecClassGenericPassword"]
        static SEC_CLASS_GENERIC_PASSWORD: Ref;
        #[link_name = "kSecAttrService"]
        static SEC_ATTR_SERVICE: Ref;
        #[link_name = "kSecAttrAccount"]
        static SEC_ATTR_ACCOUNT: Ref;
        #[link_name = "kSecAttrAccess"]
        static SEC_ATTR_ACCESS: Ref;
        #[link_name = "kSecValueData"]
        static SEC_VALUE_DATA: Ref;
    }

    /// Releases a Core Foundation object on drop.
    struct Owned(Ref);

    impl Drop for Owned {
        fn drop(&mut self) {
            if !self.0.is_null() {
                // SAFETY: a non-null object this guard owns one reference to.
                unsafe { CFRelease(self.0) };
            }
        }
    }

    fn ok(status: OsStatus) -> Result<(), BackendError> {
        if status == 0 { Ok(()) } else { Err(BackendError) }
    }

    /// The access list: this executable and the bundled helper, nothing else.
    fn access() -> Result<Owned, BackendError> {
        let exe = std::env::current_exe().map_err(|_| BackendError)?;
        let paths = trusted_application_paths(&exe).filter(|paths| paths.iter().all(|path| path.is_file()));
        let [me, updater] = paths.ok_or(BackendError)?;
        let me = CString::new(me.as_os_str().as_bytes()).map_err(|_| BackendError)?;
        let updater = CString::new(updater.as_os_str().as_bytes()).map_err(|_| BackendError)?;

        let mut me_app: Ref = ptr::null();
        let mut updater_app: Ref = ptr::null();
        // SAFETY: valid out pointer; `me` is a NUL-terminated path.
        unsafe {
            ok(SecTrustedApplicationCreateFromPath(me.as_ptr(), &mut me_app))?;
        }
        let me = Owned(me_app);
        // SAFETY: as above.
        unsafe {
            ok(SecTrustedApplicationCreateFromPath(updater.as_ptr(), &mut updater_app))?;
        }
        let updater = Owned(updater_app);

        let mut apps: [Ref; 2] = [me.0, updater.0];
        // SAFETY: `apps` holds two live CFTypes; the array retains them.
        let list = Owned(unsafe {
            CFArrayCreate(kCFAllocatorDefault, apps.as_mut_ptr(), apps.len() as CFIndex, &kCFTypeArrayCallBacks)
                .cast()
        });
        if list.0.is_null() {
            return Err(BackendError);
        }
        let label = c"Hane update restart proxy credential";
        // SAFETY: `label` is a NUL-terminated UTF-8 string.
        let label = Owned(unsafe {
            CFStringCreateWithCString(kCFAllocatorDefault, label.as_ptr(), kCFStringEncodingUTF8).cast()
        });
        if label.0.is_null() {
            return Err(BackendError);
        }
        let mut access: Ref = ptr::null();
        // SAFETY: live descriptor and list, valid out pointer.
        unsafe {
            ok(SecAccessCreate(label.0, list.0, &mut access))?;
        }
        Ok(Owned(access))
    }

    fn string(text: &str) -> Result<Owned, BackendError> {
        // SAFETY: the pointer and length describe a live UTF-8 byte slice.
        let string = Owned(unsafe {
            CFStringCreateWithBytes(
                kCFAllocatorDefault,
                text.as_ptr(),
                text.len() as CFIndex,
                kCFStringEncodingUTF8,
                0,
            )
            .cast()
        });
        if string.0.is_null() { Err(BackendError) } else { Ok(string) }
    }

    /// Stores `secret` as a generic password of `service` / `account`, created
    /// with the access list [`access`] in the same `SecItemAdd` call: the entry
    /// never exists with the default list, and no prompt or later widening is
    /// involved. `clear` removes a stale entry of the same account and is called
    /// only when the add reports a duplicate. On failure no entry is left
    /// behind. Errors carry no detail.
    pub(super) fn create(
        service: &str,
        account: &str,
        secret: &str,
        clear: impl Fn() -> Result<(), BackendError>,
    ) -> Result<(), BackendError> {
        let access = access()?;
        let service = string(service)?;
        let account = string(account)?;
        // SAFETY: the pointer and length describe the live bytes of `secret`.
        let data = Owned(unsafe {
            CFDataCreate(kCFAllocatorDefault, secret.as_ptr(), secret.len() as CFIndex).cast()
        });
        if data.0.is_null() {
            return Err(BackendError);
        }
        // SAFETY: the framework constants are valid for the process lifetime.
        let (mut keys, mut values): ([Ref; 5], [Ref; 5]) = unsafe {
            (
                [SEC_CLASS, SEC_ATTR_SERVICE, SEC_ATTR_ACCOUNT, SEC_VALUE_DATA, SEC_ATTR_ACCESS],
                [SEC_CLASS_GENERIC_PASSWORD, service.0, account.0, data.0, access.0],
            )
        };
        // SAFETY: five live keys and values; the dictionary retains them.
        let attributes = Owned(unsafe {
            CFDictionaryCreate(
                kCFAllocatorDefault,
                keys.as_mut_ptr(),
                values.as_mut_ptr(),
                keys.len() as CFIndex,
                &kCFTypeDictionaryKeyCallBacks,
                &kCFTypeDictionaryValueCallBacks,
            )
            .cast()
        });
        if attributes.0.is_null() {
            return Err(BackendError);
        }
        let add = || {
            // SAFETY: live attribute dictionary; no result is requested.
            unsafe { SecItemAdd(attributes.0, ptr::null_mut()) }
        };
        // A duplicate is retried once after the stale entry is cleared.
        let status = add();
        if status == DUPLICATE_ITEM {
            clear()?;
            return ok(add());
        }
        ok(status)
    }
}

/// No native backend: nothing can have been stored, so removing is a no-op and
/// storing or reading fails.
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub struct KeyringBackend;

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
impl SecretBackend for KeyringBackend {
    fn set(&self, _: &str, _: &str) -> std::result::Result<(), BackendError> {
        Err(BackendError)
    }

    fn get(&self, _: &str) -> std::result::Result<Option<String>, BackendError> {
        Err(BackendError)
    }

    fn delete(&self, _: &str) -> std::result::Result<(), BackendError> {
        Ok(())
    }
}

/// The production store.
pub struct OsSecretStore<B = KeyringBackend> {
    backend: B,
}

impl OsSecretStore {
    /// The store of the proxy credentials of an update restart.
    pub fn proxy() -> Self {
        Self::with_backend(KeyringBackend)
    }
}

impl<B: SecretBackend> OsSecretStore<B> {
    pub(crate) fn with_backend(backend: B) -> Self {
        Self { backend }
    }
}

/// The account name of a reference. It lives next to the other proxy entries
/// but can never equal a Manual-mode credential id.
fn account(reference: &str) -> Result<String> {
    let well_formed = !reference.is_empty()
        && reference.len() <= 128
        && reference
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'));
    if well_formed {
        Ok(format!("update-restart/{reference}"))
    } else {
        Err(failure("the temporary proxy credential reference is malformed"))
    }
}

fn failure(text: &'static str) -> UpdateError {
    UpdateError::new(ErrorCode::Io, text)
}

fn backend_failure(_: BackendError) -> UpdateError {
    failure("the OS secret store cannot handle the proxy credential")
}

impl<B: SecretBackend> TransientSecretStore for OsSecretStore<B> {
    fn put(&self, reference: &str, secret: &str) -> Result<()> {
        self.backend
            .set(&account(reference)?, secret)
            .map_err(backend_failure)
    }

    /// Reads the entry and removes it. When the removal fails the secret is
    /// not handed out (it would stay claimable); the record that names the
    /// reference stays too, so the terminal cleanup retries the removal.
    fn take(&self, reference: &str) -> Result<Option<String>> {
        let account = account(reference)?;
        let Some(secret) = self.backend.get(&account).map_err(backend_failure)? else {
            return Ok(None);
        };
        self.backend.delete(&account).map_err(backend_failure)?;
        Ok(Some(secret))
    }

    fn delete(&self, reference: &str) -> Result<()> {
        self.backend
            .delete(&account(reference)?)
            .map_err(backend_failure)
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::BTreeMap;

    use super::*;

    const DUMMY_SECRET: &str = "dummy-secret-user:dummy-secret-pass";

    #[derive(Default)]
    struct FakeBackend {
        entries: RefCell<BTreeMap<String, String>>,
        fail_set: bool,
        fail_get: bool,
        fail_delete: bool,
    }

    impl SecretBackend for FakeBackend {
        fn set(&self, account: &str, secret: &str) -> std::result::Result<(), BackendError> {
            if self.fail_set {
                return Err(BackendError);
            }
            self.entries.borrow_mut().insert(account.to_owned(), secret.to_owned());
            Ok(())
        }

        fn get(&self, account: &str) -> std::result::Result<Option<String>, BackendError> {
            if self.fail_get {
                return Err(BackendError);
            }
            Ok(self.entries.borrow().get(account).cloned())
        }

        fn delete(&self, account: &str) -> std::result::Result<(), BackendError> {
            if self.fail_delete {
                return Err(BackendError);
            }
            self.entries.borrow_mut().remove(account);
            Ok(())
        }
    }

    fn store(backend: FakeBackend) -> OsSecretStore<FakeBackend> {
        OsSecretStore::with_backend(backend)
    }

    #[test]
    fn a_secret_is_claimed_exactly_once_under_the_proxy_service_account() {
        let store = store(FakeBackend::default());
        store.put("txn.0", DUMMY_SECRET).unwrap();
        assert_eq!(
            store.backend.entries.borrow().keys().collect::<Vec<_>>(),
            ["update-restart/txn.0"]
        );
        assert_eq!(store.take("txn.0").unwrap().as_deref(), Some(DUMMY_SECRET));
        assert!(store.backend.entries.borrow().is_empty());
        assert_eq!(store.take("txn.0").unwrap(), None);
    }

    #[test]
    fn delete_is_idempotent_and_removes_the_entry() {
        let store = store(FakeBackend::default());
        store.put("txn.1", DUMMY_SECRET).unwrap();
        store.delete("txn.1").unwrap();
        store.delete("txn.1").unwrap();
        assert!(store.backend.entries.borrow().is_empty());
    }

    #[test]
    fn write_read_and_delete_failures_are_errors_without_the_secret() {
        let failing_set = store(FakeBackend { fail_set: true, ..Default::default() });
        let err = failing_set.put("txn.0", DUMMY_SECRET).unwrap_err();
        assert!(!format!("{err:?} {err}").contains("dummy-secret"));

        let failing_get = store(FakeBackend { fail_get: true, ..Default::default() });
        assert!(failing_get.take("txn.0").is_err());

        let failing_delete = store(FakeBackend { fail_delete: true, ..Default::default() });
        failing_delete
            .backend
            .entries
            .borrow_mut()
            .insert("update-restart/txn.0".to_owned(), DUMMY_SECRET.to_owned());
        // The secret is not handed out when it could not be removed, and the
        // entry stays for the cleanup to retry.
        let err = failing_delete.take("txn.0").unwrap_err();
        assert!(!format!("{err:?} {err}").contains("dummy-secret"));
        assert!(failing_delete.delete("txn.0").is_err());
        assert_eq!(failing_delete.backend.entries.borrow().len(), 1);
    }

    #[test]
    fn a_malformed_reference_never_reaches_the_os_store() {
        let store = store(FakeBackend::default());
        for reference in ["", "../escape", "a b", "txn.0\n"] {
            assert!(store.put(reference, DUMMY_SECRET).is_err(), "{reference:?}");
            assert!(store.take(reference).is_err());
            assert!(store.delete(reference).is_err());
        }
        assert!(store.backend.entries.borrow().is_empty());
    }

    #[test]
    fn the_helper_trusted_by_the_keychain_entry_is_the_bundled_one_only() {
        use std::path::Path;
        assert_eq!(
            bundled_helper_path(Path::new("/Applications/Hane.app/Contents/MacOS/hane")),
            Some(Path::new("/Applications/Hane.app/Contents/Helpers/hane-updater").to_path_buf())
        );
        // Outside the bundle layout no helper is named, so no entry is created
        // with a guessed trust list.
        for exe in [
            "/tmp/hane",
            "/tmp/target/release/hane",
            "/Applications/Hane.app/Contents/Other/hane",
            "/Applications/Hane.app/Elsewhere/MacOS/hane",
        ] {
            assert_eq!(bundled_helper_path(Path::new(exe)), None, "{exe}");
        }
    }

    #[test]
    fn the_keychain_access_list_names_exactly_the_app_and_its_bundled_helper() {
        use std::path::{Path, PathBuf};
        let exe = Path::new("/Applications/Hane.app/Contents/MacOS/hane");
        assert_eq!(
            trusted_application_paths(exe),
            Some([
                PathBuf::from("/Applications/Hane.app/Contents/MacOS/hane"),
                PathBuf::from("/Applications/Hane.app/Contents/Helpers/hane-updater"),
            ])
        );
        // Without the bundle layout there is no access list, so `put` fails and
        // the handoff aborts instead of storing under a default or wider list.
        assert_eq!(trusted_application_paths(Path::new("/tmp/hane")), None);
    }

    #[test]
    fn a_failed_put_aborts_without_a_stored_secret_or_a_fallback() {
        let store = store(FakeBackend { fail_set: true, ..Default::default() });
        assert!(store.put("txn.2", DUMMY_SECRET).is_err());
        assert!(store.backend.entries.borrow().is_empty());
        assert_eq!(store.take("txn.2").unwrap(), None);
    }

    #[cfg(any(target_os = "macos", target_os = "windows"))]
    #[test]
    fn the_native_branch_uses_the_reserved_proxy_service() {
        assert_eq!(PROXY_CREDENTIAL_SERVICE, "hane.network.proxy");
        // Absent entry: the account is well formed and the lookup is not an error
        // of the reference handling (no secret is written).
        assert!(account("00000000.0").is_ok());
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    #[test]
    fn without_a_native_backend_nothing_is_stored_and_the_handoff_fails() {
        let store = OsSecretStore::proxy();
        assert!(store.put("txn.0", DUMMY_SECRET).is_err());
        assert!(store.take("txn.0").is_err());
        // Nothing could be stored, so the cleanup has nothing to fail on.
        store.delete("txn.0").unwrap();
    }
}
