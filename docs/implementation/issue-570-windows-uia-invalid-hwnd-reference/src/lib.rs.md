# Verbatim upstream reference: src/lib.rs

Source: AccessKit accesskit_windows 0.34.0, crate SHA-256 `106c2b961215864d1c2e703ee63269c25c4e80a577ffb2c1017b9c17dcdf83a1`. License files are from repository commit `c88605b96d04431f9c3c792464a0f2f253480e94`.

Original file bytes: 501; SHA-256: `8a6f20ed5354c9834d88a59091c764f65aa31359f81c57386fe06e9d806e9396`. This is reference data, not executable project source. Preserve the bytes inside the fence when materializing an unchanged upstream file. Final newline in the source: True.

````
// Copyright 2021 The AccessKit Authors. All rights reserved.
// Licensed under the Apache License, Version 2.0 (found in
// the LICENSE-APACHE file) or the MIT license (found in
// the LICENSE-MIT file), at your option.

mod context;
mod filters;
mod node;
mod text;
mod util;
mod window_handle;

mod adapter;
pub use adapter::{Adapter, QueuedEvents};

mod subclass;
pub use subclass::SubclassingAdapter;

pub use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};

#[cfg(test)]
mod tests;
````
