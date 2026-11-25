//! The OS "reduce motion" preference (Issue #779, phase P3).
//!
//! Hane has no other reader for it: GPUI exposes nothing. It is read only at
//! the moment a band would be triggered, never on a timer, and only the band
//! honors it (drying is a color change, not motion).

/// Whether the OS asks applications to reduce motion. `false` when the
/// platform has no such setting or it cannot be read.
#[cfg(not(test))]
pub(crate) fn system_reduce_motion() -> bool {
    platform_reduce_motion()
}

/// Unit tests use GPUI's test platform, not the host's real setting.
#[cfg(test)]
pub(crate) fn system_reduce_motion() -> bool {
    false
}

#[cfg(target_os = "macos")]
#[cfg_attr(test, allow(dead_code, reason = "tests never read the host's setting"))]
fn platform_reduce_motion() -> bool {
    use objc::runtime::{BOOL, NO, Object};
    use objc::{class, msg_send, sel, sel_impl};

    #[link(name = "AppKit", kind = "framework")]
    unsafe extern "C" {}

    // `NSWorkspace.shared` is a process-wide singleton and the property is a
    // plain read of the user's accessibility setting.
    unsafe {
        let workspace: *mut Object = msg_send![class!(NSWorkspace), sharedWorkspace];
        if workspace.is_null() {
            return false;
        }
        let reduce: BOOL = msg_send![workspace, accessibilityDisplayShouldReduceMotion];
        reduce != NO
    }
}

#[cfg(target_os = "windows")]
#[cfg_attr(test, allow(dead_code, reason = "tests never read the host's setting"))]
fn platform_reduce_motion() -> bool {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        SPI_GETCLIENTAREAANIMATION, SystemParametersInfoW,
    };

    // `SPI_GETCLIENTAREAANIMATION` fills a BOOL: nonzero means animations are on.
    let mut animations: i32 = 1;
    let read = unsafe {
        SystemParametersInfoW(
            SPI_GETCLIENTAREAANIMATION,
            0,
            std::ptr::from_mut(&mut animations).cast(),
            0,
        )
    };
    read != 0 && animations == 0
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
#[cfg_attr(test, allow(dead_code, reason = "tests never read the host's setting"))]
fn platform_reduce_motion() -> bool {
    false
}
