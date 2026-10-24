//! Finder's right-click "Haneで開く" for folders, delivered as a macOS Service.
//!
//! The bundle's `NSServices` entry (see `scripts/bundle-macos.sh`) is limited to
//! folders selected in Finder and names the `openFolderInHane` message. AppKit
//! launches Hane when needed and sends `openFolderInHane:userData:error:` to
//! the services provider installed here. The selected folder goes into the same
//! queue Finder's open-URL callback fills, so the window opens it through its
//! one external-open entrance (dirty / saving sessions are refused there).
//!
//! Folders are deliberately not declared as a document type: that would change
//! Finder's "Open With" candidates and default applications.

use crate::finder_open;
use hane_ui::SharedExternalOpenQueue;
use std::ffi::{CStr, c_char, c_void};
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;
use std::sync::OnceLock;

type Id = *mut c_void;
type Sel = *mut c_void;
type Class = *mut c_void;

#[link(name = "Foundation", kind = "framework")]
unsafe extern "C" {}

#[link(name = "AppKit", kind = "framework")]
unsafe extern "C" {}

#[link(name = "objc")]
unsafe extern "C" {
    fn objc_getClass(name: *const c_char) -> Class;
    fn objc_allocateClassPair(superclass: Class, name: *const c_char, extra_bytes: usize) -> Class;
    fn objc_registerClassPair(class: Class);
    fn class_addMethod(
        class: Class,
        selector: Sel,
        implementation: unsafe extern "C" fn(Id, Sel, Id, Id, *mut Id),
        types: *const c_char,
    ) -> u8;
    fn sel_registerName(name: *const c_char) -> Sel;
    // Variadic in C; each call site casts it to the exact signature.
    fn objc_msgSend();
}

/// Sends `selector` to `receiver` through `objc_msgSend` cast to the exact C
/// signature of the method (`$ret` result, `$ty` arguments).
macro_rules! send {
    ($ret:ty; $receiver:expr, $selector:expr $(, $ty:ty = $arg:expr)*) => {{
        let call = std::mem::transmute::<
            unsafe extern "C" fn(),
            unsafe extern "C" fn(Id, Sel $(, $ty)*) -> $ret,
        >(objc_msgSend as unsafe extern "C" fn());
        call($receiver, sel_registerName($selector.as_ptr()) $(, $arg)*)
    }};
}

static QUEUE: OnceLock<SharedExternalOpenQueue> = OnceLock::new();

/// Registers the services provider on the running application. Must run after
/// the application object exists (inside the GPUI run callback).
pub(crate) fn install(queue: SharedExternalOpenQueue) {
    if QUEUE.set(queue).is_err() {
        return;
    }
    // SAFETY: plain Objective-C runtime calls with matching signatures. The
    // provider is leaked on purpose: the application keeps it for its lifetime.
    unsafe {
        let class = objc_allocateClassPair(
            objc_getClass(c"NSObject".as_ptr()),
            c"HaneFolderServiceProvider".as_ptr(),
            0,
        );
        if class.is_null() {
            return;
        }
        class_addMethod(
            class,
            sel_registerName(c"openFolderInHane:userData:error:".as_ptr()),
            open_folder_in_hane,
            c"v@:@@^@".as_ptr(),
        );
        objc_registerClassPair(class);
        let provider = send!(Id; class, c"alloc");
        let provider = send!(Id; provider, c"init");
        let application = send!(Id; objc_getClass(c"NSApplication".as_ptr()), c"sharedApplication");
        send!((); application, c"setServicesProvider:", Id = provider);
    }
}

/// `-openFolderInHane:(NSPasteboard *)pasteboard userData:(NSString *)userData
/// error:(NSString **)error`.
unsafe extern "C" fn open_folder_in_hane(
    _this: Id,
    _selector: Sel,
    pasteboard: Id,
    _user_data: Id,
    error: *mut Id,
) {
    // SAFETY: AppKit passes a live NSPasteboard for the duration of the call.
    let paths = unsafe { selected_paths(pasteboard) };
    let folder = finder_open::single_folder(&paths);
    let Some(folder) = folder else {
        if !error.is_null() {
            // SAFETY: `error` is the caller's out-parameter.
            unsafe { *error = ns_string(c"Hane can open exactly one existing folder at a time.") };
        }
        return;
    };
    let Some(queue) = QUEUE.get() else {
        return;
    };
    // A sealed queue means the update helper is already authorized to replace
    // this process, so the folder is handed to the process that follows.
    if let Err(folder) = queue.try_push(folder) {
        crate::hand_off_to_next_process(&folder);
    }
}

unsafe fn ns_string(text: &CStr) -> Id {
    // SAFETY: `text` is NUL-terminated.
    unsafe {
        send!(Id; objc_getClass(c"NSString".as_ptr()), c"stringWithUTF8String:", *const c_char = text.as_ptr())
    }
}

unsafe fn string_bytes(string: Id) -> Option<PathBuf> {
    if string.is_null() {
        return None;
    }
    // SAFETY: `string` is a live NSString.
    let utf8 = unsafe { send!(*const c_char; string, c"UTF8String") };
    if utf8.is_null() {
        return None;
    }
    // SAFETY: `UTF8String` is NUL-terminated and valid for this autorelease scope.
    let bytes = unsafe { CStr::from_ptr(utf8) }.to_bytes();
    Some(PathBuf::from(std::ffi::OsStr::from_bytes(bytes)))
}

/// The file paths on the service pasteboard: the legacy file-name list first,
/// then a single `public.file-url` as a fallback.
unsafe fn selected_paths(pasteboard: Id) -> Vec<PathBuf> {
    if pasteboard.is_null() {
        return Vec::new();
    }
    // SAFETY: Objective-C message sends on live Foundation / AppKit objects.
    unsafe {
        let list = send!(
            Id; pasteboard, c"propertyListForType:",
            Id = ns_string(c"NSFilenamesPboardType")
        );
        if !list.is_null() {
            let count = send!(usize; list, c"count");
            return (0..count)
                .filter_map(|index| {
                    string_bytes(send!(Id; list, c"objectAtIndex:", usize = index))
                })
                .collect();
        }
        let url = send!(Id; pasteboard, c"stringForType:", Id = ns_string(c"public.file-url"));
        string_bytes(url)
            .and_then(|url| url.to_str().and_then(finder_open::openable_path_from_url))
            .into_iter()
            .collect()
    }
}
