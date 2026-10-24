# Verbatim upstream reference: src/window_handle.rs

Source: AccessKit accesskit_windows 0.34.0, crate SHA-256 `106c2b961215864d1c2e703ee63269c25c4e80a577ffb2c1017b9c17dcdf83a1`. License files are from repository commit `c88605b96d04431f9c3c792464a0f2f253480e94`.

Original file bytes: 753; SHA-256: `dec1d78c81784a18c50f066e89d57d0fe02a7c9199a11e47a0833f8b9113d8c2`. This is reference data, not executable project source. Preserve the bytes inside the fence when materializing an unchanged upstream file. Final newline in the source: True.

````
// Copyright 2024 The AccessKit Authors. All rights reserved.
// Licensed under the Apache License, Version 2.0 (found in
// the LICENSE-APACHE file) or the MIT license (found in
// the LICENSE-MIT file), at your option.

use windows::Win32::Foundation::HWND;

#[repr(transparent)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WindowHandle(pub HWND);

unsafe impl Send for WindowHandle {}
unsafe impl Sync for WindowHandle {}

impl From<HWND> for WindowHandle {
    fn from(value: HWND) -> Self {
        Self(value)
    }
}

impl From<WindowHandle> for HWND {
    fn from(value: WindowHandle) -> Self {
        value.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    static_assertions::assert_impl_all!(WindowHandle: Send, Sync);
}
````
