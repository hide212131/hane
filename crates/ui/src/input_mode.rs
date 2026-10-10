/// The mode displayed beside Hane's caret. This is independent of GPUI's
/// platform backend so that the GPUI snapshot can be upgraded as one unit.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum KeyboardInputMode {
    Ascii,
    Native,
}

/// Updates the mode shown beside the caret and reports whether the view needs
/// to redraw. An unknown mode remains unknown instead of being shown as ASCII.
pub(crate) fn update_keyboard_input_mode(
    current: &mut Option<KeyboardInputMode>,
    observed: Option<KeyboardInputMode>,
) -> bool {
    if *current == observed {
        return false;
    }

    *current = observed;
    true
}

// Unit tests use GPUI's test platform, not the host's actual input source.
#[cfg(test)]
pub(crate) fn active_keyboard_input_mode() -> Option<KeyboardInputMode> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn updates_only_when_the_observed_mode_changes() {
        let mut current = Some(KeyboardInputMode::Ascii);

        assert!(update_keyboard_input_mode(
            &mut current,
            Some(KeyboardInputMode::Native)
        ));
        assert_eq!(current, Some(KeyboardInputMode::Native));
        assert!(!update_keyboard_input_mode(
            &mut current,
            Some(KeyboardInputMode::Native)
        ));
    }

    #[test]
    fn unknown_mode_hides_the_badge_and_can_be_recovered() {
        let mut current = Some(KeyboardInputMode::Native);

        assert!(update_keyboard_input_mode(&mut current, None));
        assert_eq!(current, None);
        assert!(!update_keyboard_input_mode(&mut current, None));
        assert!(update_keyboard_input_mode(
            &mut current,
            Some(KeyboardInputMode::Ascii)
        ));
        assert_eq!(current, Some(KeyboardInputMode::Ascii));
    }
}

#[cfg(all(not(test), target_os = "macos"))]
pub(crate) fn active_keyboard_input_mode() -> Option<KeyboardInputMode> {
    use std::ffi::c_void;

    #[link(name = "Carbon", kind = "framework")]
    unsafe extern "C" {
        fn TISCopyCurrentKeyboardInputSource() -> *const c_void;
        fn TISGetInputSourceProperty(
            source: *const c_void,
            property: *const c_void,
        ) -> *const c_void;
        static kTISPropertyInputSourceIsASCIICapable: *const c_void;
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFBooleanGetValue(value: *const c_void) -> u8;
        fn CFRelease(value: *const c_void);
    }

    // Carbon's Copy rule transfers ownership of the input source to us; its
    // property is borrowed and must be read before the source is released.
    unsafe {
        let source = TISCopyCurrentKeyboardInputSource();
        if source.is_null() {
            return None;
        }
        let property = TISGetInputSourceProperty(source, kTISPropertyInputSourceIsASCIICapable);
        let mode = if property.is_null() {
            None
        } else if CFBooleanGetValue(property) != 0 {
            Some(KeyboardInputMode::Ascii)
        } else {
            Some(KeyboardInputMode::Native)
        };
        CFRelease(source);
        mode
    }
}

#[cfg(all(not(test), target_os = "windows"))]
pub(crate) fn active_keyboard_input_mode() -> Option<KeyboardInputMode> {
    use windows_sys::Win32::UI::Input::{Ime::*, KeyboardAndMouse::GetActiveWindow};

    unsafe {
        let window = GetActiveWindow();
        if window.is_null() {
            return None;
        }
        let context = ImmGetContext(window);
        if context.is_null() {
            return None;
        }
        let mode = if ImmGetOpenStatus(context) == 0 {
            Some(KeyboardInputMode::Ascii)
        } else {
            let mut conversion = 0;
            let mut sentence = 0;
            (ImmGetConversionStatus(context, &mut conversion, &mut sentence) != 0).then_some({
                if conversion & (IME_CMODE_NATIVE | IME_CMODE_NATIVESYMBOL) != 0 {
                    KeyboardInputMode::Native
                } else {
                    KeyboardInputMode::Ascii
                }
            })
        };
        ImmReleaseContext(window, context);
        mode
    }
}

#[cfg(all(not(test), not(any(target_os = "macos", target_os = "windows"))))]
pub(crate) fn active_keyboard_input_mode() -> Option<KeyboardInputMode> {
    None
}
